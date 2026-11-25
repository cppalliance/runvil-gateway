//! Tests for `POST /admin/chatgpt/models/enable` and
//! `POST /admin/chatgpt/models/forget`: happy path, idempotency, name
//! collision handling (`<name>-chatgpt` and double-clash), and forget's
//! preservation of multi-endpoint fallback models.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as AxumStatus, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use gateway_chatgpt_plan::fake_issuer::Fake;
use gateway_config::Config;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{Rig, call, error_code};
use crate::chatgpt::{ChatGpt, PLAN_PROOF_HEADER};
use crate::test_support::{AdminPaths, app_state};

#[derive(Clone, Default)]
pub(super) struct ModelsApi {
    state: Arc<Mutex<ModelsBehavior>>,
    pub(super) entered: Arc<Notify>,
    pub(super) release: Arc<Notify>,
}

pub(super) struct ModelsBehavior {
    pub(super) status: u16,
    pub(super) body: String,
    pub(super) redirect_to: Option<String>,
    pub(super) extra_header: Option<(String, String)>,
    pub(super) fail_first_401: bool,
    pub(super) hold: bool,
    pub(super) hits: u32,
    pub(super) auth_headers: Vec<String>,
}

impl Default for ModelsBehavior {
    fn default() -> Self {
        Self {
            status: 200,
            body: json!({
                "models": [
                    {
                        "slug": "gpt-5",
                        "display_name": "GPT-5",
                        "visibility": "list",
                        "context_window": 400_000
                    },
                    {
                        "slug": "gpt-5-mini",
                        "visibility": "list",
                        "context_window": 200_000
                    },
                    {
                        "slug": "gpt-hidden",
                        "display_name": "Hidden Internal",
                        "visibility": "hide",
                        "context_window": 128_000
                    },
                    {
                        "slug": "gpt-no-ctx",
                        "display_name": "No Context",
                        "visibility": "list"
                    }
                ]
            })
            .to_string(),
            redirect_to: None,
            extra_header: None,
            fail_first_401: false,
            hold: false,
            hits: 0,
            auth_headers: Vec::new(),
        }
    }
}

impl ModelsApi {
    pub(super) async fn start() -> (Self, SocketAddr) {
        let api = Self::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("models listener binds");
        let addr = listener.local_addr().expect("bound addr");
        let served = api.clone();
        tokio::spawn(async move {
            let app = Router::new()
                .route("/v1/models", get(handle_models))
                .with_state(served);
            let _ = axum::serve(listener, app).await;
        });
        (api, addr)
    }

    pub(super) fn set(&self, f: impl FnOnce(&mut ModelsBehavior)) {
        f(&mut self.state.lock().expect("models lock"));
    }

    pub(super) fn hits(&self) -> u32 {
        self.state.lock().expect("models lock").hits
    }

    pub(super) fn auth_headers(&self) -> Vec<String> {
        self.state.lock().expect("models lock").auth_headers.clone()
    }
}

async fn handle_models(State(api): State<ModelsApi>, headers: HeaderMap) -> Response {
    let (hold, reply) = {
        let mut guard = api.state.lock().expect("models lock");
        guard.hits += 1;
        if let Some(auth) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
        {
            guard.auth_headers.push(auth.to_owned());
        }
        if guard.fail_first_401 {
            guard.fail_first_401 = false;
            return (AxumStatus::UNAUTHORIZED, "expired token").into_response();
        }
        let hold = guard.hold;
        let response = if let Some(loc) = guard.redirect_to.clone() {
            (AxumStatus::FOUND, [(header::LOCATION, loc)], String::new()).into_response()
        } else {
            let status = AxumStatus::from_u16(guard.status).expect("status");
            let mut resp = (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                guard.body.clone(),
            )
                .into_response();
            if let Some((k, v)) = &guard.extra_header {
                resp.headers_mut().insert(
                    axum::http::HeaderName::from_bytes(k.as_bytes()).expect("header name"),
                    axum::http::HeaderValue::from_str(v).expect("header val"),
                );
            }
            resp
        };
        (hold, response)
    };
    if hold {
        api.entered.notify_one();
        api.release.notified().await;
    }
    reply
}

pub(super) struct ModelRig {
    pub(super) rig: Rig,
    pub(super) api: ModelsApi,
    pub(super) config_path: PathBuf,
    pub(super) state: crate::AppState,
}

impl ModelRig {
    pub(super) async fn with_toml(extra_toml: &str) -> Self {
        let fake = Fake::start().await;
        let (api, api_addr) = ModelsApi::start().await;
        let profile = tempfile::TempDir::new().expect("tempdir");
        let cache = profile.path().join("cache");
        std::fs::create_dir_all(cache.join("models")).expect("mkdir cache");
        let config_path = profile.path().join("gateway.toml");
        let base_toml = format!(
            "config-version = 0\n\n\
             [server]\n\
             bind = \"127.0.0.1:0\"\n\
             api_key = \"test-token\"\n\n\
             [local]\n\
             cache_dir = '{}'\n\n\
             {extra_toml}",
            cache.display()
        );
        std::fs::write(&config_path, &base_toml).expect("write gateway.toml");
        let config = Config::from_toml_str(&base_toml).expect("valid base config");
        let mut state = app_state(
            config,
            Some(AdminPaths {
                fixture_dir: profile.path().to_path_buf(),
                active: "main".to_owned(),
                config_path: config_path.clone(),
            }),
        );
        state.chatgpt = ChatGpt::with_api_base(&format!("http://{api_addr}/v1"));
        state.chatgpt.open_on(profile.path(), fake.config());
        let rig = Rig::serve(fake, profile, state.clone()).await;
        Self {
            rig,
            api,
            config_path,
            state,
        }
    }

    pub(super) async fn sign_in(&self) {
        let callback = self.rig.begin().await;
        let page = reqwest::get(&callback).await.expect("callback answers");
        assert_eq!(page.status(), StatusCode::OK);
    }

    pub(super) async fn listed_models(&self, with_plan_proof: bool) -> Vec<String> {
        let mut req = reqwest::Client::new()
            .get(format!("http://{}/v1/models", self.rig.addr))
            .bearer_auth("test-token");
        if with_plan_proof {
            req = req.header(PLAN_PROOF_HEADER, &self.rig.plan_proof);
        }
        let body: Value = req
            .send()
            .await
            .expect("v1/models")
            .json()
            .await
            .expect("json");
        body["data"]
            .as_array()
            .expect("data array")
            .iter()
            .map(|entry| entry["id"].as_str().expect("id").to_owned())
            .collect()
    }
}

#[tokio::test]
async fn enable_adds_visible_plan_models_applies_immediately_and_is_idempotent() {
    let env = ModelRig::with_toml("").await;
    env.sign_in().await;

    let response = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.expect("enable reply");
    assert_eq!(
        reply,
        json!({
            "endpoint": "chatgpt",
            "added": ["gpt-5", "gpt-5-mini"],
            "renamed": [],
            "skipped": [{ "name": "gpt-no-ctx", "reason": "missing_context_window" }],
        })
    );

    // Both live routing and status update immediately without a restart.
    assert_eq!(
        env.rig.status().await["plan_models"],
        json!(["gpt-5", "gpt-5-mini"])
    );
    assert_eq!(env.listed_models(true).await, ["gpt-5", "gpt-5-mini"]);
    assert!(env.listed_models(false).await.is_empty());

    // Rollback copy `.prev` was created before replacing `gateway.toml`.
    assert!(
        env.config_path
            .with_file_name("gateway.toml.prev")
            .is_file()
    );
    let disk = std::fs::read_to_string(&env.config_path).expect("read config");
    assert!(disk.contains("auth = \"chatgpt-plan\""), "{disk}");
    assert!(disk.contains("description = \"GPT-5\""), "{disk}");
    assert!(!disk.contains("gpt-hidden"), "{disk}");

    // Stamp a comment on disk, then run enable a second time: no-op writes
    // nothing to disk and reports `added: []`.
    let commented = format!("# keep-on-noop\n{disk}");
    std::fs::write(&env.config_path, &commented).expect("write comment");
    let second = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_reply: Value = second.json().await.expect("second reply");
    assert_eq!(
        second_reply,
        json!({
            "endpoint": "chatgpt",
            "added": [],
            "renamed": [],
            "skipped": [{ "name": "gpt-no-ctx", "reason": "missing_context_window" }],
        })
    );
    let after_second = std::fs::read_to_string(&env.config_path).expect("read after second");
    assert!(
        after_second.starts_with("# keep-on-noop\n"),
        "idempotent no-op should not rewrite unchanged gateway.toml"
    );
}

#[tokio::test]
async fn enable_suffixes_clashing_names_and_skips_double_clashes() {
    let extra = r#"
[[endpoint]]
id = "paid"
protocol = "openai"
base_url = "http://127.0.0.1:9/v1"
api_key = "sk-paid"

[[model]]
name = "gpt-5"
description = "Paid GPT-5"
upstream = "gpt-5"
endpoints = ["paid"]
context = 128000

[[model]]
name = "gpt-4o"
description = "Paid GPT-4o"
upstream = "gpt-4o"
endpoints = ["paid"]
context = 128000

[[model]]
name = "gpt-4o-chatgpt"
description = "Custom suffixed GPT-4o"
upstream = "gpt-4o"
endpoints = ["paid"]
context = 128000
"#;
    let env = ModelRig::with_toml(extra).await;
    env.api.set(|b| {
        b.body = json!({
            "models": [
                { "slug": "gpt-5", "display_name": "GPT-5", "visibility": "list", "context_window": 400_000 },
                { "slug": "gpt-4o", "display_name": "GPT-4o", "visibility": "list", "context_window": 128_000 },
                { "slug": "o3", "display_name": "o3", "visibility": "list", "context_window": 200_000 }
            ]
        })
        .to_string();
    });
    env.sign_in().await;

    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body,
        json!({
            "endpoint": "chatgpt",
            "added": ["gpt-5-chatgpt", "o3"],
            "renamed": [{ "from": "gpt-5", "to": "gpt-5-chatgpt" }],
            "skipped": [{ "name": "gpt-4o", "reason": "name_taken" }],
        })
    );
    assert_eq!(
        env.rig.status().await["plan_models"],
        json!(["gpt-5-chatgpt", "o3"])
    );
    let disk = std::fs::read_to_string(&env.config_path).expect("read config");
    assert!(disk.contains("upstream = \"gpt-5\""), "{disk}");

    // A repeat call recognizes the suffixed binding via upstream and adds nothing.
    let again = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(again.status(), StatusCode::OK);
    let again_body: Value = again.json().await.expect("json");
    assert_eq!(again_body["added"], json!([]));
    assert_eq!(again_body["renamed"], json!([]));
    assert_eq!(
        again_body["skipped"],
        json!([{ "name": "gpt-4o", "reason": "name_taken" }])
    );
}

#[tokio::test]
async fn forget_removes_plan_endpoint_and_plan_only_models_while_keeping_fallbacks() {
    let extra = r#"
[[endpoint]]
id = "paid"
protocol = "openai"
base_url = "http://127.0.0.1:9/v1"
api_key = "sk-paid"

[[endpoint]]
id = "chatgpt"
protocol = "openai-responses"
auth = "chatgpt-plan"

[[model]]
name = "gpt-5"
description = "GPT-5"
upstream = "gpt-5"
endpoints = ["chatgpt"]
context = 400000

[[model]]
name = "hybrid"
description = "Hybrid"
upstream = "gpt-5"
endpoints = ["chatgpt", "paid"]
context = 128000
"#;
    let env = ModelRig::with_toml(extra).await;
    // Works even when signed out (no active sign-in needed to clean config).
    assert_eq!(
        env.rig.status().await["plan_models"],
        json!(["gpt-5", "hybrid"])
    );

    let resp = env.rig.owner(Method::POST, "models/forget").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("forget reply");
    assert_eq!(
        body,
        json!({
            "removed_endpoints": ["chatgpt"],
            "removed": ["gpt-5"],
            "kept": ["hybrid"],
        })
    );
    assert_eq!(env.rig.status().await["plan_models"], json!([]));
    assert_eq!(env.listed_models(false).await, ["hybrid"]);

    let disk = std::fs::read_to_string(&env.config_path).expect("read config");
    assert!(!disk.contains("chatgpt-plan"), "{disk}");
    assert!(!disk.contains("name = \"gpt-5\""), "{disk}");
    assert!(disk.contains("name = \"hybrid\""), "{disk}");

    // Second forget is a clean no-op.
    let second = env.rig.owner(Method::POST, "models/forget").await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.json::<Value>().await.expect("json"),
        json!({ "removed_endpoints": [], "removed": [], "kept": [] })
    );
}

#[tokio::test]
async fn api_first_mixed_endpoint_row_gets_a_plan_variant_instead_of_being_skipped() {
    let extra = r#"
[[endpoint]]
id = "paid"
protocol = "openai"
base_url = "http://127.0.0.1:9/v1"
api_key = "sk-paid"

[[endpoint]]
id = "chatgpt"
protocol = "openai-responses"
auth = "chatgpt-plan"

[[model]]
name = "gpt-5"
description = "Paid primary"
upstream = "gpt-5"
endpoints = ["paid", "chatgpt"]
context = 128000
"#;
    let env = ModelRig::with_toml(extra).await;
    env.sign_in().await;
    assert_eq!(
        env.rig.status().await["plan_models"],
        json!([]),
        "endpoints[0] is API-key backed, so no plan model is active yet"
    );

    env.api.set(|b| {
        b.body = json!({
            "models": [{
                "slug": "gpt-5",
                "display_name": "GPT-5",
                "visibility": "list",
                "context_window": 400_000
            }]
        })
        .to_string();
    });

    let resp = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("enable reply");
    assert_eq!(
        body,
        json!({
            "endpoint": "chatgpt",
            "added": ["gpt-5-chatgpt"],
            "renamed": [{ "from": "gpt-5", "to": "gpt-5-chatgpt" }],
            "skipped": [],
        })
    );
    assert_eq!(
        env.rig.status().await["plan_models"],
        json!(["gpt-5-chatgpt"])
    );

    // Repeat enable is now idempotent because gpt-5-chatgpt binds endpoints[0] = "chatgpt".
    let again = env.rig.owner(Method::POST, "models/enable").await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(
        again.json::<Value>().await.expect("json")["added"],
        json!([])
    );
}

#[path = "models-tests-guards.rs"]
mod guards;
