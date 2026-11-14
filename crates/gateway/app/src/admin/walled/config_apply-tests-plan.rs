//! End-to-end admin config round-trips with `auth = "chatgpt-plan"` endpoints:
//! unchanged `GET -> PUT -> POST /admin/config-apply`, unrelated edits and
//! reverts, switching an ordinary endpoint to `chatgpt-plan` and back, and
//! profile selection (`POST /admin/switch-profile` + `GET /admin/config-pending`).

use std::net::SocketAddr;
use std::path::Path;

use gateway_config::{Config, EndpointAuth, ProfileSelection, profile_state_path, shadow_path};
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::{get_json, post, save_edited, serve_fixture};
use crate::AppState;
use crate::routing::CredentialSource;
use crate::test_support::AdminPaths;

const PLAN_CONFIG: &str = r#"
config-version = 0

[server]
bind = "127.0.0.1:0"
api_key = "test-token"

[[endpoint]]
id = "plan"
protocol = "openai-responses"
auth = "chatgpt-plan"

[[endpoint]]
id = "own"
protocol = "openai"
base_url = "http://127.0.0.1:9"
api_key = "own-secret"

[[model]]
name = "plan-model"
description = "plan chat"
context = 8192
upstream = "gpt-plan"
endpoints = ["plan"]

[[model]]
name = "own-model"
description = "own chat"
context = 8192
upstream = "gpt-own"
endpoints = ["own"]

[[profile]]
name = "alpha"
models = []

[[profile]]
name = "beta"
models = []
"#;

fn plan_fixture() -> (tempfile::TempDir, Config, AdminPaths) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let config_path = temp.path().join("gateway.toml");
    std::fs::write(&config_path, PLAN_CONFIG).expect("write config");
    std::fs::write(
        profile_state_path(&config_path),
        "active_profile = \"alpha\"\n",
    )
    .expect("write state");
    let config = Config::load(&config_path, &ProfileSelection::default()).expect("load config");
    let paths = AdminPaths {
        fixture_dir: temp.path().to_path_buf(),
        active: "alpha".to_owned(),
        config_path,
    };
    (temp, config, paths)
}

fn assert_plan_json(endpoint: &Value, expected_id: &str) {
    let obj = endpoint.as_object().expect("endpoint is object");
    assert_eq!(obj.get("id"), Some(&json!(expected_id)));
    assert_eq!(obj.get("protocol"), Some(&json!("openai-responses")));
    assert_eq!(obj.get("auth"), Some(&json!("chatgpt-plan")));
    assert!(
        !obj.contains_key("base_url"),
        "plan endpoint JSON must omit base_url: {obj:?}"
    );
    assert!(
        !obj.contains_key("api_key"),
        "plan endpoint JSON must omit api_key: {obj:?}"
    );
}

fn assert_plan_toml_on_disk(path: &Path, expected_id: &str) {
    let raw = std::fs::read_to_string(path).expect("read config file");
    let parsed: toml::Value = toml::from_str(&raw).expect("parse TOML");
    let endpoints = parsed["endpoint"].as_array().expect("endpoint array");
    let plan = endpoints
        .iter()
        .find(|item| item.get("id").and_then(toml::Value::as_str) == Some(expected_id))
        .expect("plan endpoint in TOML")
        .as_table()
        .expect("endpoint table");
    assert_eq!(
        plan.get("auth").and_then(toml::Value::as_str),
        Some("chatgpt-plan")
    );
    assert_eq!(
        plan.get("protocol").and_then(toml::Value::as_str),
        Some("openai-responses")
    );
    assert!(
        !plan.contains_key("base_url"),
        "plan endpoint TOML must never gain base_url: {plan:?}"
    );
    assert!(
        !plan.contains_key("api_key"),
        "plan endpoint TOML must never gain api_key: {plan:?}"
    );
}

async fn listed_models(addr: SocketAddr) -> Vec<String> {
    let body = get_json(addr, "v1/models").await;
    body["data"]
        .as_array()
        .expect("models data array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("model id").to_owned())
        .collect()
}

async fn endpoint_source(state: &AppState, model_name: &str) -> CredentialSource {
    state
        .live
        .read()
        .await
        .routing
        .model(model_name)
        .expect("model routes")
        .endpoint
        .source
}

#[tokio::test]
async fn get_put_unchanged_and_apply_round_trips_a_plan_endpoint_without_url_or_key() {
    let (_temp, config, paths) = plan_fixture();
    let config_path = paths.config_path.clone();
    let (addr, state) = serve_fixture(config, paths).await;

    // 1. GET /admin/config emits the plan endpoint without base_url or api_key.
    let get_resp = reqwest::Client::new()
        .get(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .send()
        .await
        .expect("get sends");
    let etag = get_resp.headers()[reqwest::header::ETAG]
        .to_str()
        .expect("ETag header")
        .to_owned();
    let initial: serde_json::Value = get_resp.json().await.expect("json body");
    assert_plan_json(&initial["endpoint"][0], "plan");
    assert_eq!(initial["endpoint"][1]["api_key"], "***");

    // 2. PUT the exact JSON back unchanged with its ETag precondition.
    let put = reqwest::Client::new()
        .put(format!("http://{addr}/admin/config"))
        .bearer_auth("test-token")
        .header(reqwest::header::IF_MATCH, etag)
        .json(&initial)
        .send()
        .await
        .expect("put sends");
    assert_eq!(put.status(), StatusCode::OK);
    assert_plan_toml_on_disk(&shadow_path(&config_path), "plan");

    // 3. GET /admin/config-pending and GET /admin/config-dirty stay clean on endpoint.
    let pending = get_json(addr, "admin/config-pending").await;
    assert_eq!(pending["profile"]["active_profile"], "alpha");
    assert_plan_json(&pending["profile"]["endpoint"][0], "plan");
    let dirty = get_json(addr, "admin/config-dirty").await;
    assert_eq!(dirty["dirty"], true);
    assert_eq!(dirty["pending_files"], json!(["gateway.toml"]));

    // 4. POST /admin/config-apply promotes the shadow and reloads routing.
    let apply = post(addr, "admin/config-apply").await;
    assert_eq!(apply.status(), StatusCode::OK);
    assert!(!shadow_path(&config_path).exists());
    assert_plan_toml_on_disk(&config_path, "plan");

    let reloaded = Config::load(&config_path, &ProfileSelection::default())
        .expect("promoted gateway.toml loads cleanly");
    assert_eq!(reloaded.endpoints()[0].auth(), EndpointAuth::ChatgptPlan);
    assert!(reloaded.endpoints()[0].api_key().is_none());
    assert_eq!(
        reloaded.endpoints()[1].api_key().expect("own key").expose(),
        "own-secret"
    );

    assert_eq!(
        endpoint_source(&state, "plan-model").await,
        CredentialSource::ChatgptPlan
    );
    assert_eq!(listed_models(addr).await, vec!["own-model"]);
    let after = get_json(addr, "admin/config").await;
    assert_plan_json(&after["endpoint"][0], "plan");
}

#[tokio::test]
async fn editing_unrelated_fields_and_reverting_or_applying_never_corrupts_plan_endpoint() {
    let (_temp, config, paths) = plan_fixture();
    let config_path = paths.config_path.clone();
    let (addr, _state) = serve_fixture(config, paths).await;

    let first_save = save_edited(addr, |body| {
        body["model"][1]["description"] = json!("edited own chat");
    })
    .await;
    assert_eq!(first_save.status(), StatusCode::OK);
    assert_plan_toml_on_disk(&shadow_path(&config_path), "plan");

    let revert = post(addr, "admin/config-revert").await;
    assert_eq!(revert.status(), StatusCode::OK);
    assert!(!shadow_path(&config_path).exists());

    let second_save = save_edited(addr, |body| {
        body["model"][0]["description"] = json!("edited plan chat");
        body["local"]["cache_dir"] = json!("~/.runvil-alt");
    })
    .await;
    assert_eq!(second_save.status(), StatusCode::OK);
    let apply = post(addr, "admin/config-apply").await;
    assert_eq!(apply.status(), StatusCode::OK);
    assert_plan_toml_on_disk(&config_path, "plan");

    let after = get_json(addr, "admin/config").await;
    assert_eq!(after["model"][0]["description"], "edited plan chat");
    assert_plan_json(&after["endpoint"][0], "plan");
}

#[tokio::test]
async fn switching_ordinary_endpoint_to_plan_and_back_updates_live_gating_and_rejects_stale_redactions()
 {
    let (_temp, config, paths) = plan_fixture();
    let config_path = paths.config_path.clone();
    let (addr, state) = serve_fixture(config, paths).await;
    assert_eq!(listed_models(addr).await, vec!["own-model"]);

    // Switching "own" to chatgpt-plan while leaving `"api_key": "***"` is refused.
    let stale_redacted = save_edited(addr, |body| {
        body["endpoint"][1]["protocol"] = json!("openai-responses");
        body["endpoint"][1]["auth"] = json!("chatgpt-plan");
        body["endpoint"][1]
            .as_object_mut()
            .expect("endpoint object")
            .remove("base_url");
    })
    .await;
    assert_eq!(stale_redacted.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!shadow_path(&config_path).exists());

    // Switching "own" cleanly to chatgpt-plan drops base_url and api_key and gates own-model.
    let to_plan = save_edited(addr, |body| {
        body["endpoint"][1] = json!({
            "id": "own",
            "protocol": "openai-responses",
            "auth": "chatgpt-plan",
            "dominion": null,
        });
    })
    .await;
    assert_eq!(to_plan.status(), StatusCode::OK);
    assert_eq!(
        post(addr, "admin/config-apply").await.status(),
        StatusCode::OK
    );
    assert_plan_toml_on_disk(&config_path, "own");
    assert_eq!(
        endpoint_source(&state, "own-model").await,
        CredentialSource::ChatgptPlan
    );
    assert!(listed_models(addr).await.is_empty());

    let gated_chat = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth("test-token")
        .json(&json!({
            "model": "own-model",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .expect("chat sends");
    assert_eq!(gated_chat.status(), StatusCode::FORBIDDEN);
    let gated_err: Value = gated_chat.json().await.expect("error json");
    assert_eq!(gated_err["error"]["code"], "chatgpt_plan_runvil_only");

    // Switching "own" back to ordinary with `"api_key": "***"` fails closed.
    let bad_back = save_edited(addr, |body| {
        body["endpoint"][1] = json!({
            "id": "own",
            "protocol": "openai",
            "base_url": "http://127.0.0.1:9",
            "api_key": "***",
            "dominion": null,
        });
    })
    .await;
    assert_eq!(bad_back.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Switching "own" back with explicit URL and key restores Configured routing.
    let valid_back = save_edited(addr, |body| {
        body["endpoint"][1] = json!({
            "id": "own",
            "protocol": "openai",
            "base_url": "http://127.0.0.1:9",
            "api_key": "fresh-secret",
            "dominion": null,
        });
    })
    .await;
    assert_eq!(valid_back.status(), StatusCode::OK);
    assert_eq!(
        post(addr, "admin/config-apply").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        endpoint_source(&state, "own-model").await,
        CredentialSource::Configured
    );
    assert_eq!(listed_models(addr).await, vec!["own-model"]);
}

#[tokio::test]
async fn switch_profile_and_pending_view_preserve_plan_endpoint_across_selections() {
    let (_temp, config, paths) = plan_fixture();
    let config_path = paths.config_path.clone();
    let (addr, _state) = serve_fixture(config, paths).await;

    let switch = reqwest::Client::new()
        .post(format!("http://{addr}/admin/switch-profile"))
        .bearer_auth("test-token")
        .json(&json!({ "name": "beta" }))
        .send()
        .await
        .expect("switch-profile sends");
    assert_eq!(switch.status(), StatusCode::OK);
    let switch_body: Value = switch.json().await.expect("switch json");
    assert_eq!(
        switch_body,
        json!({ "profile": "beta", "restart_required": true })
    );

    let pending = get_json(addr, "admin/config-pending").await;
    assert_eq!(pending["profile"]["active_profile"], "beta");
    assert_plan_json(&pending["profile"]["endpoint"][0], "plan");

    // Saving a new profile checklist while "beta" is selected keeps the plan endpoint clean.
    let save = save_edited(addr, |body| {
        body["profile"]
            .as_array_mut()
            .expect("profiles array")
            .push(json!({ "name": "gamma", "models": [] }));
    })
    .await;
    assert_eq!(save.status(), StatusCode::OK);
    assert_eq!(
        post(addr, "admin/config-apply").await.status(),
        StatusCode::OK
    );
    assert_plan_toml_on_disk(&config_path, "plan");
    assert_eq!(
        get_json(addr, "admin/profiles").await["profiles"],
        json!(["alpha", "beta", "gamma"])
    );
}
