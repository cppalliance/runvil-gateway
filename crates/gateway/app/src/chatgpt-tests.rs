//! The gate on plan-backed models, over real HTTP: who may use one, who may
//! see one, and what a plan request carries when it is let through.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use gateway_chatgpt_plan::fake_issuer::Fake;
use gateway_config::Config;
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use super::{ChatGpt, OWNER_PROOF_HEADER, PLAN_PROOF_HEADER};
use crate::AppState;
use crate::routing::CredentialSource;
use crate::test_support::{app_state, serve_state};

/// One request a fake backend received: its `Authorization` header and body.
type Received = Arc<Mutex<Vec<(String, Value)>>>;

/// A backend that records each `POST /responses` and answers 401, so a
/// test reads what was sent without imitating a model.
async fn recording_backend() -> (String, Received) {
    let received: Received = Arc::default();
    let record = {
        let received = Arc::clone(&received);
        move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
            let authorization = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            received.lock().expect("lock").push((authorization, body));
            std::future::ready((
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "error": { "message": "no", "code": "invalid_api_key" } })),
            ))
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds");
    let addr = listener.local_addr().expect("an address");
    tokio::spawn(async move {
        let router = axum::Router::new().route("/responses", axum::routing::post(record));
        let _ignored = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), received)
}

/// A gateway with two models that share one upstream name: `sol` on a
/// ChatGPT plan, `own-sol` on an ordinary API key.
struct Rig {
    fake: Fake,
    profile: tempfile::TempDir,
    state: AppState,
    received: Received,
}

impl Rig {
    async fn new() -> Rig {
        let (backend, received) = recording_backend().await;
        let config = Config::from_toml_str(&format!(
            r#"
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
protocol = "openai-responses"
base_url = "{backend}"
api_key = "own-key"

[[model]]
name = "sol"
description = "on the plan"
context = 8192
upstream = "gpt-x"
endpoints = ["plan"]

[[model]]
name = "own-sol"
description = "on a key"
context = 8192
upstream = "gpt-x"
endpoints = ["own"]
"#
        ))
        .expect("the config parses");
        let mut state = app_state(config, None);
        state.chatgpt = ChatGpt::with_api_base(&backend);
        Rig {
            fake: Fake::start().await,
            profile: tempfile::TempDir::new().expect("tempdir"),
            state,
            received,
        }
    }

    /// Opens the broker on the fake issuer and serves the gateway.
    async fn serve(&self) -> SocketAddr {
        self.state
            .chatgpt
            .open_on(self.profile.path(), self.fake.config());
        serve_state(self.state.clone()).await
    }

    fn plan_proof(&self) -> String {
        self.state.chatgpt.plan_proof().to_owned()
    }

    fn sent(&self) -> Vec<(String, Value)> {
        self.received.lock().expect("lock").clone()
    }

    /// Signs in through the admin routes and the fake issuer.
    async fn sign_in(&self, addr: SocketAddr) {
        let reply: Value = reqwest::Client::new()
            .post(format!("http://{addr}/admin/chatgpt/sign-in"))
            .bearer_auth("test-token")
            .header(OWNER_PROOF_HEADER, self.state.chatgpt.owner_proof())
            .send()
            .await
            .expect("the gateway answers")
            .json()
            .await
            .expect("a sign-in reply");
        let query: HashMap<String, String> =
            Url::parse(reply["authorize_url"].as_str().expect("a url"))
                .expect("a url")
                .query_pairs()
                .into_owned()
                .collect();
        self.fake
            .set(|behavior| behavior.nonce.clone_from(&query["nonce"]));
        self.fake
            .expect_exchange(&query["code_challenge"], &query["redirect_uri"]);
        let mut callback = Url::parse(&query["redirect_uri"]).expect("a redirect uri");
        callback
            .query_pairs_mut()
            .append_pair("state", &query["state"])
            .append_pair("code", "code-1")
            .append_pair("client_id", "oaiapp_1");
        let page = reqwest::get(callback).await.expect("the listener answers");
        assert_eq!(page.status(), StatusCode::OK);
    }
}

/// `POST`s `body` to `path` with the bearer and one plan-proof header per
/// entry of `proofs`.
async fn post(addr: SocketAddr, path: &str, body: &Value, proofs: &[&str]) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .bearer_auth("test-token")
        .json(body);
    for proof in proofs {
        request = request.header(PLAN_PROOF_HEADER, *proof);
    }
    request.send().await.expect("the gateway answers")
}

async fn refusal(response: reqwest::Response) -> (StatusCode, String) {
    let status = response.status();
    let body: Value = response.json().await.expect("an error envelope");
    (
        status,
        body["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

async fn listed(addr: SocketAddr, proofs: &[&str]) -> Vec<String> {
    let mut request = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .bearer_auth("test-token");
    for proof in proofs {
        request = request.header(PLAN_PROOF_HEADER, *proof);
    }
    let body: Value = request
        .send()
        .await
        .expect("the gateway answers")
        .json()
        .await
        .expect("a catalog");
    body["data"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect()
}

fn chat(model: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": "hi" }] })
}

/// Every inference route, with a valid body naming `model`.
fn every_route(model: &str) -> Vec<(&'static str, Value)> {
    let mut streamed = chat(model);
    streamed["stream"] = json!(true);
    vec![
        ("/v1/chat/completions", chat(model)),
        ("/v1/chat/completions", streamed),
        ("/v1/embeddings", json!({ "model": model, "input": "hi" })),
        (
            "/v1/rerank",
            json!({ "model": model, "query": "q", "documents": ["a"] }),
        ),
        (
            "/v1/audio/speech",
            json!({ "model": model, "input": "hi", "voice": "alloy" }),
        ),
    ]
}

#[tokio::test]
async fn a_plan_model_refuses_every_route_without_the_plan_proof() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    rig.sign_in(addr).await;
    let proof = rig.plan_proof();
    let owner_proof = rig.state.chatgpt.owner_proof().to_owned();
    let wrong = "0".repeat(64);
    let attempts: [&[&str]; 4] = [
        &[],
        &[&wrong],
        // The owner proof is not the plan proof.
        &[&owner_proof],
        // A repeated header counts as none, even when both are right.
        &[&proof, &proof],
    ];
    for (path, body) in every_route("sol") {
        for proofs in attempts {
            let outcome = refusal(post(addr, path, &body, proofs).await).await;
            assert_eq!(
                outcome,
                (StatusCode::FORBIDDEN, "chatgpt_plan_runvil_only".to_owned()),
                "{path} {body}"
            );
        }
    }
    // Nothing was spent: no request left for the API, and the broker was
    // asked for no token beyond the sign-in's own exchange.
    assert!(rig.sent().is_empty());
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn the_plan_proof_alone_is_not_a_credential() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    let proof = rig.plan_proof();
    // A LAN-shaped caller: the proof as the bearer, and no key at all.
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth(&proof)
        .header(PLAN_PROOF_HEADER, &proof)
        .json(&chat("sol"))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_authorized_request_spends_the_plan_at_the_plan_destination() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    let proof = rig.plan_proof();

    // Signed out: the gate passes, the broker has no token, nothing leaves.
    let outcome = refusal(post(addr, "/v1/chat/completions", &chat("sol"), &[&proof]).await).await;
    assert_eq!(
        outcome,
        (StatusCode::FORBIDDEN, "chatgpt_sign_in_required".to_owned())
    );
    assert!(rig.sent().is_empty());

    rig.sign_in(addr).await;
    let mut body = chat("sol");
    body["temperature"] = json!(0.2);
    post(addr, "/v1/chat/completions", &body, &[&proof]).await;

    let sent = rig.sent();
    let [(authorization, request)] = sent.as_slice() else {
        panic!("one request reaches the API: {sent:?}");
    };
    let token = authorization.strip_prefix("Bearer ").expect("a bearer");
    assert!(!token.is_empty());
    for not_it in ["own-key", "test-token", proof.as_str()] {
        assert_ne!(token, not_it);
    }
    assert!(
        !rig.fake
            .seen(|seen| seen.valid_refresh_tokens.contains(token)),
        "a refresh token is never sent to the API"
    );
    // The plan's request rules, not the caller's parameters.
    assert_eq!(request["model"], "gpt-x");
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
    assert!(request.get("temperature").is_none(), "{request}");
}

#[tokio::test]
async fn an_ordinary_model_with_the_same_upstream_name_is_untouched() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    post(addr, "/v1/chat/completions", &chat("own-sol"), &[]).await;
    let sent = rig.sent();
    let [(authorization, request)] = sent.as_slice() else {
        panic!("one request reaches the backend: {sent:?}");
    };
    assert_eq!(authorization, "Bearer own-key");
    assert_eq!(request["model"], "gpt-x");
    assert!(rig.fake.grants().is_empty());
}

#[tokio::test]
async fn the_catalog_lists_a_plan_model_only_for_runvils_client() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    let proof = rig.plan_proof();
    // Signed out: nobody sees the plan model, even with the plan proof.
    assert_eq!(listed(addr, &[&proof]).await, ["own-sol"]);
    assert_eq!(rig.fake.seen(|seen| seen.discovery_hits), 0);

    rig.sign_in(addr).await;
    let hits_before = rig.fake.seen(|seen| seen.discovery_hits);
    assert_eq!(listed(addr, &[]).await, ["own-sol"]);
    assert_eq!(listed(addr, &[&proof, &proof]).await, ["own-sol"]);
    assert_eq!(listed(addr, &[&proof]).await, ["sol", "own-sol"]);
    // Listing asked the issuer and the API for nothing.
    assert_eq!(rig.fake.seen(|seen| seen.discovery_hits), hits_before);
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn a_gateway_beyond_loopback_serves_no_plan_model() {
    let rig = Rig::new().await;
    // What the listener actually bound decides it: a wildcard here.
    let bound = "0.0.0.0:8081".parse().expect("an address");
    rig.state.chatgpt.start(bound, Some(rig.profile.path()));
    let addr = serve_state(rig.state.clone()).await;
    let proof = rig.plan_proof();
    for proofs in [&[][..], &[proof.as_str()][..]] {
        for (path, body) in every_route("sol") {
            let outcome = refusal(post(addr, path, &body, proofs).await).await;
            assert_eq!(
                outcome,
                (
                    StatusCode::FORBIDDEN,
                    "chatgpt_plan_loopback_only".to_owned()
                ),
                "{path}"
            );
        }
        assert_eq!(listed(addr, proofs).await, ["own-sol"]);
    }
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn a_gateway_that_cannot_hold_a_sign_in_says_so() {
    let rig = Rig::new().await;
    rig.state.chatgpt.unavailable("a stated reason");
    let addr = serve_state(rig.state.clone()).await;
    let proof = rig.plan_proof();
    let outcome = refusal(post(addr, "/v1/chat/completions", &chat("sol"), &[&proof]).await).await;
    assert_eq!(
        outcome,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "chatgpt_unavailable".to_owned()
        )
    );
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn the_catalog_entry_of_a_plan_model_spends_nothing() {
    let rig = Rig::new().await;
    let addr = rig.serve().await;
    rig.sign_in(addr).await;
    let model = rig
        .state
        .live
        .read()
        .await
        .routing
        .model("sol")
        .expect("configured");
    assert_eq!(model.endpoint.source, CredentialSource::ChatgptPlan);
    let request: crate::wire::ChatRequest = serde_json::from_value(chat("sol")).expect("a request");
    // A handler that skipped the resolver holds only this.
    let sent = model.endpoint.upstream.send(request.clone(), "gpt-x").await;
    assert!(sent.is_err());
    assert!(
        model
            .endpoint
            .upstream
            .stream(request, "gpt-x")
            .await
            .is_err()
    );
    assert!(rig.sent().is_empty());
}

#[test]
fn only_a_loopback_listener_holds_a_sign_in() {
    for (bound, holds) in [
        ("127.0.0.1:8081", true),
        ("127.0.0.2:8081", true),
        ("[::1]:8081", true),
        ("0.0.0.0:8081", false),
        ("[::]:8081", false),
        ("192.168.1.20:8081", false),
    ] {
        let profile = tempfile::TempDir::new().expect("tempdir");
        let chatgpt = ChatGpt::new();
        chatgpt.start(bound.parse().expect("an address"), Some(profile.path()));
        assert_eq!(chatgpt.broker().is_ok(), holds, "{bound}");
        // The store is created only where a sign-in can be held.
        assert_eq!(
            profile.path().join("chatgpt-plan").exists(),
            holds,
            "{bound}"
        );
    }
    let chatgpt = ChatGpt::new();
    chatgpt.start("127.0.0.1:8081".parse().expect("an address"), None);
    assert!(chatgpt.broker().is_err());
}

#[test]
fn a_development_name_is_taken_only_when_it_is_plain() {
    for (raw, name) in [
        ("RunvilDevAgent", Some("RunvilDevAgent")),
        ("  Runvil (dev) ", Some("Runvil (dev)")),
        ("", None),
        ("   ", None),
        ("Runvil<script>", None),
        ("Runvil\nOpenAI", None),
        ("Runvil\u{202e}", None),
    ] {
        assert_eq!(super::agent_name(raw).as_deref(), name, "{raw:?}");
    }
    assert_eq!(
        super::agent_name(&"a".repeat(64)).map(|name| name.len()),
        Some(64)
    );
    assert_eq!(super::agent_name(&"a".repeat(65)), None);

    // Only the name changes: the issuer and the API stay OpenAI's.
    let production = super::oauth_config_named(None);
    assert_eq!(production.agent_name, "Runvil");
    for (requested, name) in [("RunvilDevAgent", "RunvilDevAgent"), ("<b>x</b>", "Runvil")] {
        let config = super::oauth_config_named(Some(requested));
        assert_eq!(config.agent_name, name);
        assert_eq!(config.issuer, production.issuer);
        assert_eq!(config.resource, production.resource);
    }
}

#[path = "chatgpt-tests-visibility.rs"]
mod visibility;
