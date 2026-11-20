//! The `/admin/chatgpt/*` routes over real HTTP, against the real broker
//! and the strict fake issuer.

use std::collections::HashMap;
use std::net::SocketAddr;

use gateway_chatgpt_plan::fake_issuer::Fake;
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;

use crate::AppState;
use crate::chatgpt::OWNER_PROOF_HEADER;
use crate::test_support::{loopback_state, serve_state, walled_fixture};

const ROUTES: [(Method, &str); 7] = [
    (Method::POST, "sign-in"),
    (Method::GET, "status"),
    (Method::POST, "sign-out"),
    (Method::POST, "forget"),
    (Method::POST, "reset"),
    (Method::POST, "models/enable"),
    (Method::POST, "models/forget"),
];

/// A served gateway whose broker is open on a fake issuer.
struct Rig {
    fake: Fake,
    _profile: tempfile::TempDir,
    addr: SocketAddr,
    owner_proof: String,
    plan_proof: String,
}

impl Rig {
    async fn new() -> Rig {
        let fake = Fake::start().await;
        let (profile, state) = walled_fixture();
        state.chatgpt.open_on(profile.path(), fake.config());
        Rig::serve(fake, profile, state).await
    }

    async fn serve(fake: Fake, profile: tempfile::TempDir, state: AppState) -> Rig {
        let owner_proof = state.chatgpt.owner_proof().to_owned();
        let plan_proof = state.chatgpt.plan_proof().to_owned();
        Rig {
            fake,
            _profile: profile,
            addr: serve_state(state).await,
            owner_proof,
            plan_proof,
        }
    }

    /// Calls a route as the owner: the bearer and this boot's owner proof.
    async fn owner(&self, method: Method, route: &str) -> reqwest::Response {
        call(
            self.addr,
            method,
            route,
            Some("test-token"),
            &[&self.owner_proof],
        )
        .await
    }

    async fn status(&self) -> Value {
        let response = self.owner(Method::GET, "status").await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.expect("a status object")
    }

    /// Starts a sign-in, teaches the fake that attempt, and returns the
    /// callback URL the browser would be sent back to.
    async fn begin(&self) -> String {
        let response = self.owner(Method::POST, "sign-in").await;
        assert_eq!(response.status(), StatusCode::OK);
        let reply: Value = response.json().await.expect("a sign-in reply");
        let authorize_url = reply["authorize_url"].as_str().expect("a url");
        assert!(
            authorize_url.starts_with(&self.fake.base),
            "{authorize_url}"
        );
        let query: HashMap<String, String> = Url::parse(authorize_url)
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
        callback.into()
    }
}

/// Calls `/admin/chatgpt/<route>` with an optional bearer and one owner
/// proof header per entry of `proofs`.
async fn call(
    addr: SocketAddr,
    method: Method,
    route: &str,
    bearer: Option<&str>,
    proofs: &[&str],
) -> reqwest::Response {
    let mut request =
        reqwest::Client::new().request(method, format!("http://{addr}/admin/chatgpt/{route}"));
    if let Some(bearer) = bearer {
        request = request.bearer_auth(bearer);
    }
    for proof in proofs {
        request = request.header(OWNER_PROOF_HEADER, *proof);
    }
    request.send().await.expect("the gateway answers")
}

async fn error_code(response: reqwest::Response) -> String {
    let body: Value = response.json().await.expect("an error envelope");
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn every_route_refuses_a_caller_without_the_owner_proof() {
    let rig = Rig::new().await;
    for (method, route) in ROUTES {
        let wrong = "0".repeat(64);
        let attempts: [&[&str]; 4] = [
            &[],
            &[&wrong],
            // The plan proof is not the owner proof.
            &[&rig.plan_proof],
            // A repeated header counts as none, even when both are right.
            &[&rig.owner_proof, &rig.owner_proof],
        ];
        for proofs in attempts {
            let response = call(rig.addr, method.clone(), route, Some("test-token"), proofs).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{route} {proofs:?}"
            );
            assert_eq!(
                error_code(response).await,
                "owner_proof_required",
                "{route}"
            );
        }
    }
    assert_eq!(rig.fake.seen(|seen| seen.discovery_hits), 0);
    assert_eq!(rig.status().await["sign_in_pending"], false);
}

#[tokio::test]
async fn the_owner_proof_does_not_stand_in_for_authentication() {
    // Strict bearer auth: no keyless loopback.
    let state = loopback_state(Some(false));
    let owner_proof = state.chatgpt.owner_proof().to_owned();
    let addr = serve_state(state).await;
    for (method, route) in ROUTES {
        let response = call(addr, method, route, None, &[&owner_proof]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{route}");
    }
}

#[tokio::test]
async fn status_says_why_sign_in_is_unavailable() {
    let (_temp, state) = walled_fixture();
    let owner_proof = state.chatgpt.owner_proof().to_owned();
    let unopened = serve_state(state.clone()).await;
    let response = call(
        unopened,
        Method::GET,
        "status",
        Some("test-token"),
        &[&owner_proof],
    )
    .await;
    let status: Value = response.json().await.expect("a status object");
    assert_eq!(status["available"], false);
    assert_eq!(status["unavailable_code"], "starting");
    assert_eq!(
        status["unavailable_reason"],
        "the gateway has not finished starting"
    );
    assert_eq!(status["plan_models"], serde_json::json!([]));

    state.chatgpt.unavailable("a stated reason");
    let addr = serve_state(state).await;
    let response = call(
        addr,
        Method::GET,
        "status",
        Some("test-token"),
        &[&owner_proof],
    )
    .await;
    let status: Value = response.json().await.expect("a status object");
    assert_eq!(status["available"], false);
    assert_eq!(status["unavailable_code"], "storage_unavailable");
    assert_eq!(status["unavailable_reason"], "a stated reason");
    assert_eq!(status["status"], Value::Null);
    assert_eq!(status["plan_models"], serde_json::json!([]));

    // Non-loopback and missing-profile-dir carry distinct codes for the UI.
    let (_temp_lan, lan_state) = walled_fixture();
    let lan_proof = lan_state.chatgpt.owner_proof().to_owned();
    lan_state
        .chatgpt
        .start("0.0.0.0:8081".parse().expect("addr"), None);
    let lan_addr = serve_state(lan_state).await;
    let lan_status: Value = call(
        lan_addr,
        Method::GET,
        "status",
        Some("test-token"),
        &[&lan_proof],
    )
    .await
    .json()
    .await
    .expect("status");
    assert_eq!(lan_status["unavailable_code"], "not_loopback");

    let (_temp_nop, nop_state) = walled_fixture();
    let nop_proof = nop_state.chatgpt.owner_proof().to_owned();
    nop_state
        .chatgpt
        .start("127.0.0.1:8081".parse().expect("addr"), None);
    let nop_addr = serve_state(nop_state).await;
    let nop_status: Value = call(
        nop_addr,
        Method::GET,
        "status",
        Some("test-token"),
        &[&nop_proof],
    )
    .await
    .json()
    .await
    .expect("status");
    assert_eq!(nop_status["unavailable_code"], "no_profile_dir");

    for (method, route) in ROUTES {
        if route == "status" {
            continue;
        }
        let response = call(addr, method, route, Some("test-token"), &[&owner_proof]).await;
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route}"
        );
        assert_eq!(error_code(response).await, "chatgpt_unavailable", "{route}");
    }
}

#[tokio::test]
async fn a_fresh_profile_reports_nobody_signed_in() {
    let rig = Rig::new().await;
    let status = rig.status().await;
    assert_eq!(
        status,
        serde_json::json!({
            "status": null,
            "unreadable": false,
            "email": null,
            "plan_granted": false,
            "revocation": null,
            "cleanup_unconfirmed": false,
            "sign_in_pending": false,
            "available": true,
            "unavailable_code": null,
            "unavailable_reason": null,
            "plan_models": [],
        })
    );
    let response = rig.owner(Method::POST, "sign-out").await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.expect("a reply");
    assert_eq!(reply, serde_json::json!({ "revoked": false }));
    assert!(rig.fake.grants().is_empty());
}

#[tokio::test]
async fn a_sign_in_and_sign_out_run_end_to_end_through_the_routes() {
    let rig = Rig::new().await;
    let callback = rig.begin().await;
    assert_eq!(rig.status().await["sign_in_pending"], true);

    // The browser lands on the one-shot listener.
    let page = reqwest::get(&callback).await.expect("the listener answers");
    assert_eq!(page.status(), StatusCode::OK);
    let page = page.text().await.expect("a page");

    let status = rig.status().await;
    assert_eq!(status["status"], "active", "{status}");
    assert_eq!(status["plan_granted"], true);
    assert_eq!(status["sign_in_pending"], false);
    assert!(status["email"].is_string(), "{status}");
    assert_eq!(rig.fake.grants(), ["authorization_code"]);

    // Nothing the routes or the page return holds a token.
    let issued = rig.fake.seen(|seen| seen.valid_refresh_tokens.clone());
    assert!(!issued.is_empty());
    for token in &issued {
        assert!(!status.to_string().contains(token) && !page.contains(token));
    }

    // The listener was one-shot: it is gone.
    assert!(reqwest::get(&callback).await.is_err());

    let response = rig.owner(Method::POST, "sign-out").await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.expect("a reply");
    assert_eq!(reply, serde_json::json!({ "revoked": true }));
    let status = rig.status().await;
    assert_eq!(status["status"], "signed_out", "{status}");
    assert_eq!(status["revocation"], "confirmed");
    assert_eq!(rig.fake.seen(|seen| seen.revoked.len()), 1);
}

#[tokio::test]
async fn a_second_sign_in_replaces_the_first() {
    let rig = Rig::new().await;
    let first = rig.begin().await;
    let second = rig.begin().await;
    assert_ne!(first, second);

    // The first attempt's listener closed when the second replaced it: its
    // port stops answering, and it exchanges nothing.
    let mut closed = false;
    for _ in 0..200 {
        closed = reqwest::get(&first).await.is_err();
        if closed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(closed, "a replaced sign-in must not keep its port");
    assert!(rig.fake.grants().is_empty());

    let page = reqwest::get(&second).await.expect("the listener answers");
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(rig.status().await["status"], "active");
    assert_eq!(rig.fake.grants(), ["authorization_code"]);
}

#[tokio::test]
async fn reset_clears_the_profile_and_answers_no_content() {
    let rig = Rig::new().await;
    let callback = rig.begin().await;
    reqwest::get(&callback).await.expect("the listener answers");
    assert_eq!(rig.status().await["status"], "active");

    let response = rig.owner(Method::POST, "reset").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(rig.status().await["status"], Value::Null);
}

#[tokio::test]
async fn unconfirmed_revocation_remains_visible_in_status_after_new_account_activates() {
    let rig = Rig::new().await;
    let first_cb = rig.begin().await;
    assert_eq!(
        reqwest::get(&first_cb).await.expect("cb").status(),
        StatusCode::OK
    );
    assert_eq!(rig.status().await["status"], "active");

    rig.fake.set(|behavior| behavior.revoke_status = 503);
    let forget = rig.owner(Method::POST, "forget").await;
    assert_eq!(forget.status(), StatusCode::OK);
    assert_eq!(
        forget.json::<Value>().await.expect("json"),
        serde_json::json!({ "revoked": false })
    );

    rig.fake.set(|behavior| {
        behavior.revoke_status = 200;
        behavior.subject = "user-2".to_owned();
    });
    let second_cb = rig.begin().await;
    assert_eq!(
        reqwest::get(&second_cb).await.expect("cb").status(),
        StatusCode::OK
    );

    let status = rig.status().await;
    assert_eq!(status["status"], "active", "{status}");
    assert_eq!(
        status["cleanup_unconfirmed"], true,
        "an unconfirmed revocation from the retired account stays reported while the new account is active: {status}"
    );
}

#[path = "models-tests.rs"]
mod models;
