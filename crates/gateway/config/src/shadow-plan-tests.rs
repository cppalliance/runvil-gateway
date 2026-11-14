//! Shadow save, pending load, dirty report, and `${VAR}` reference tests for
//! `auth = "chatgpt-plan"` endpoints alongside ordinary endpoints.

use super::*;
use crate::EndpointAuth;

const PLAN_AND_KEY_CONFIG: &str = r#"
config-version = 0

[server]
bind = "127.0.0.1:8081"
api_key = "server-secret"

[[endpoint]]
id = "plan"
protocol = "openai-responses"
auth = "chatgpt-plan"

[[endpoint]]
id = "own"
protocol = "openai"
base_url = "https://api.example.test/v1"
api_key = "${RV_SHADOW_PLAN_TEST_KEY}"

[[model]]
name = "plan-chat"
description = "on the plan"
context = 8192
upstream = "gpt-plan"
endpoints = ["plan"]

[[model]]
name = "own-chat"
description = "on an api key"
context = 8192
upstream = "gpt-own"
endpoints = ["own"]

[[local_model]]
name = "local"
description = "local model"
source = "/models/local.gguf"
context = 4096

[[profile]]
name = "work"
models = ["local"]

[[profile]]
name = "light"
models = []
"#;

fn test_env_lookup(name: &str) -> Result<String, std::env::VarError> {
    if name == "RV_SHADOW_PLAN_TEST_KEY" {
        return Ok("sk-from-env".to_owned());
    }
    Err(std::env::VarError::NotPresent)
}

fn write_plan_config() -> (tempfile::TempDir, PathBuf, crate::Config) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let path = temp.path().join("gateway.toml");
    fs::write(&path, PLAN_AND_KEY_CONFIG).expect("write config");
    let config = load_pending_config(&path, &ProfileSelection::default(), &test_env_lookup)
        .expect("initial config loads");
    (temp, path, config)
}

/// Converts a JSON value to TOML the same way `PUT /admin/config` does:
/// `null` object members drop out and arrays/tables recurse.
fn json_to_toml(value: serde_json::Value) -> Option<Value> {
    Some(match value {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(flag) => Value::Boolean(flag),
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Value::Integer(integer)
            } else {
                Value::Float(number.as_f64().expect("finite float"))
            }
        }
        serde_json::Value::String(text) => Value::String(text),
        serde_json::Value::Array(items) => {
            Value::Array(items.into_iter().filter_map(json_to_toml).collect())
        }
        serde_json::Value::Object(members) => {
            let mut table = toml::map::Map::new();
            for (key, member) in members {
                if let Some(converted) = json_to_toml(member) {
                    table.insert(key, converted);
                }
            }
            Value::Table(table)
        }
    })
}

fn assert_plan_table_has_no_url_or_key(shadow_toml: &str, id: &str) {
    let parsed: Value = toml::from_str(shadow_toml).expect("shadow parses as TOML");
    let endpoints = parsed["endpoint"].as_array().expect("endpoint array");
    let plan = endpoints
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
        .expect("plan endpoint exists in shadow");
    let table = plan.as_table().expect("endpoint is a table");
    assert_eq!(
        table.get("auth").and_then(Value::as_str),
        Some("chatgpt-plan")
    );
    assert_eq!(
        table.get("protocol").and_then(Value::as_str),
        Some("openai-responses")
    );
    assert!(
        !table.contains_key("base_url"),
        "plan endpoint must never gain base_url: {table:?}"
    );
    assert!(
        !table.contains_key("api_key"),
        "plan endpoint must never gain api_key: {table:?}"
    );
}

#[test]
fn unchanged_to_json_round_trips_through_save_config_shadow_and_load_pending() {
    let (_temp, path, config) = write_plan_config();
    let wire = config.to_json();
    let candidate = json_to_toml(wire).expect("config json converts to toml");

    let shadows = save_config_shadow(&path, candidate, &test_env_lookup)
        .expect("unchanged JSON saves cleanly");

    let shadow_raw = fs::read_to_string(&shadows.config).expect("read shadow");
    assert_plan_table_has_no_url_or_key(&shadow_raw, "plan");
    assert!(
        shadow_raw.contains("${RV_SHADOW_PLAN_TEST_KEY}"),
        "the sibling endpoint keeps its ${{VAR}} reference through *** restoration"
    );

    let pending = load_pending_config(&path, &ProfileSelection::default(), &test_env_lookup)
        .expect("pending config loads");
    let plan = pending
        .endpoints()
        .iter()
        .find(|endpoint| endpoint.id() == "plan")
        .expect("plan endpoint loaded");
    assert_eq!(plan.auth(), EndpointAuth::ChatgptPlan);
    assert!(plan.api_key().is_none());
    assert_eq!(plan.base_url(), "");

    let pending_json = pending.to_json();
    let plan_json = pending_json["endpoint"][0]
        .as_object()
        .expect("plan endpoint json");
    assert_eq!(
        plan_json.get("auth"),
        Some(&serde_json::json!("chatgpt-plan"))
    );
    assert!(!plan_json.contains_key("base_url"));
    assert!(!plan_json.contains_key("api_key"));
}

#[test]
fn editing_an_unrelated_field_preserves_plan_endpoint_and_var_references() {
    let (_temp, path, config) = write_plan_config();
    let mut wire = config.to_json();
    wire["model"][0]["description"] = serde_json::json!("updated plan description");
    let candidate = json_to_toml(wire).expect("config json converts to toml");

    let shadows = save_config_shadow(&path, candidate, &test_env_lookup).expect("save succeeds");
    let shadow_raw = fs::read_to_string(&shadows.config).expect("read shadow");
    assert_plan_table_has_no_url_or_key(&shadow_raw, "plan");

    let references = pending_var_references(&path).expect("var references");
    assert_eq!(
        references.get("RV_SHADOW_PLAN_TEST_KEY"),
        Some(&vec!["endpoint own api_key".to_owned()]),
        "only the ordinary endpoint references an env var"
    );
    let report = pending_report(&path).expect("pending report");
    assert_eq!(report.shadowed_files, std::slice::from_ref(&path));
    assert!(
        !report
            .changed_sections
            .iter()
            .any(|section| section == "endpoint"),
        "the endpoint section did not change relative to canonical TOML: {:?}",
        report.changed_sections
    );
}

#[test]
fn switching_an_ordinary_endpoint_to_plan_drops_url_and_secret_and_switching_back_requires_new_credentials()
 {
    let (_temp, path, config) = write_plan_config();
    let mut to_plan = config.to_json();
    to_plan["endpoint"][1] = serde_json::json!({
        "id": "own",
        "protocol": "openai-responses",
        "auth": "chatgpt-plan",
        "dominion": null,
    });

    let shadows = save_config_shadow(
        &path,
        json_to_toml(to_plan).expect("converts"),
        &test_env_lookup,
    )
    .expect("switching ordinary endpoint to plan succeeds");
    let shadow_raw = fs::read_to_string(&shadows.config).expect("read shadow");
    assert_plan_table_has_no_url_or_key(&shadow_raw, "own");
    assert!(
        !shadow_raw.contains("RV_SHADOW_PLAN_TEST_KEY") && !shadow_raw.contains("api.example.test"),
        "switching to chatgpt-plan strips the previous URL and key from the shadow"
    );

    let pending_plan = load_pending_config(&path, &ProfileSelection::default(), &test_env_lookup)
        .expect("pending loads");
    assert!(
        pending_plan
            .endpoints()
            .iter()
            .all(|endpoint| endpoint.auth() == EndpointAuth::ChatgptPlan)
    );

    // Switching back from chatgpt-plan to ordinary while sending `"***"` must
    // fail closed because the plan shadow holds no secret to preserve.
    let mut redacted_back = pending_plan.to_json();
    redacted_back["endpoint"][1] = serde_json::json!({
        "id": "own",
        "protocol": "openai",
        "base_url": "https://api.example.test/v1",
        "api_key": "***",
        "dominion": null,
    });
    let redacted_err = save_config_shadow(
        &path,
        json_to_toml(redacted_back).expect("converts"),
        &test_env_lookup,
    )
    .expect_err("a plan endpoint has no secret to restore from ***");
    assert_eq!(redacted_err.kind(), crate::ConfigErrorKind::Validation);
    assert!(
        redacted_err.to_string().contains("endpoint own.api_key"),
        "unexpected error: {redacted_err}"
    );

    // Supplying real credentials switches the endpoint back to ApiKey cleanly.
    let mut valid_back = pending_plan.to_json();
    valid_back["endpoint"][1] = serde_json::json!({
        "id": "own",
        "protocol": "openai",
        "base_url": "https://api.example.test/v1",
        "api_key": "sk-fresh",
        "dominion": null,
    });
    save_config_shadow(
        &path,
        json_to_toml(valid_back).expect("converts"),
        &test_env_lookup,
    )
    .expect("switching back with explicit credentials succeeds");
    let restored = load_pending_config(&path, &ProfileSelection::default(), &test_env_lookup)
        .expect("restored loads");
    let own = restored
        .endpoints()
        .iter()
        .find(|endpoint| endpoint.id() == "own")
        .expect("own endpoint");
    assert_eq!(own.auth(), EndpointAuth::ApiKey);
    assert_eq!(own.base_url(), "https://api.example.test/v1");
    assert_eq!(own.api_key().expect("api_key").expose(), "sk-fresh");
}

#[test]
fn plan_endpoint_refuses_redacted_or_empty_key_and_url_without_writing_a_shadow() {
    let (_temp, path, config) = write_plan_config();
    for bad_endpoint in [
        serde_json::json!({
            "id": "plan",
            "protocol": "openai-responses",
            "auth": "chatgpt-plan",
            "base_url": "",
        }),
        serde_json::json!({
            "id": "plan",
            "protocol": "openai-responses",
            "auth": "chatgpt-plan",
            "api_key": "",
        }),
        serde_json::json!({
            "id": "plan",
            "protocol": "openai-responses",
            "auth": "chatgpt-plan",
            "api_key": "***",
        }),
    ] {
        let mut wire = config.to_json();
        wire["endpoint"][0] = bad_endpoint.clone();
        let err = save_config_shadow(
            &path,
            json_to_toml(wire).expect("converts"),
            &test_env_lookup,
        )
        .expect_err("plan endpoint with URL or key must be refused");
        assert_eq!(
            err.kind(),
            crate::ConfigErrorKind::Validation,
            "{bad_endpoint}: {err}"
        );
        assert!(
            !shadow_path(&path).exists(),
            "no shadow written for {bad_endpoint}"
        );
    }
}
