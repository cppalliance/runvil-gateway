//! A fake OpenAI sign-in service: discovery, JWKS, the token endpoint (code
//! exchange and refresh), and revocation, over real HTTP with real RS256
//! ID tokens. Tests steer it through [`Behavior`] and read what it saw.
//!
//! It is strict where the real service is: a code exchange must carry the
//! documented fields and the PKCE verifier for the challenge the authorize
//! URL carried, a refresh token works once, and a revoked one stops
//! working. A client that sends the wrong thing fails here too.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Form, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::sync::Notify;

use crate::testing::{at_hash, jwks_document, sign};

/// Which token requests the fake holds open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// Code exchanges.
    Exchange,
    /// Refreshes.
    Refresh,
    /// Revocations.
    Revoke,
}

/// How the fake answers. Every field has a well-behaved default.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each omit_* switch independently toggles one optional protocol field"
)]
pub struct Behavior {
    /// The account every ID token names.
    pub subject: String,
    /// The nonce the next code exchange's ID token carries.
    pub nonce: String,
    /// The scopes every token response grants.
    pub scope: String,
    /// The access token lifetime every response reports.
    pub expires_in: u64,
    /// Leave the ID token out of code exchanges.
    pub omit_id_token: bool,
    /// Leave the ID token out of refresh responses. The real service sends
    /// one on every refresh.
    pub omit_id_token_on_refresh: bool,
    /// The key id the ID tokens are signed under, and the JWKS lists.
    pub kid: String,
    /// Leave `scope` out of refresh responses, as RFC 6749 allows.
    pub omit_scope_on_refresh: bool,
    /// Leave `refresh_token` out of refresh responses, keeping the presented
    /// refresh token live (non-rotating refresh, as RFC 6749 allows).
    pub omit_refresh_token_on_refresh: bool,
    /// The discovery document's status.
    pub discovery_status: u16,
    /// The JWKS endpoint's status.
    pub jwks_status: u16,
    /// Answer the token endpoint with this status and OAuth error.
    pub token_error: Option<(u16, &'static str)>,
    /// Answer the token endpoint with a redirect to this URL.
    pub token_redirect: Option<String>,
    /// Hold every token request of this kind until [`Fake::release`] is
    /// called.
    pub hold: Option<Held>,
    /// The revocation endpoint's status.
    pub revoke_status: u16,
    /// The `issuer` the discovery document names, when not the fake itself.
    pub discovery_issuer: Option<String>,
    /// The token endpoint the discovery document lists, when not the fake's.
    pub discovery_token_endpoint: Option<String>,
}

impl Default for Behavior {
    fn default() -> Behavior {
        Behavior {
            subject: "user-1".to_owned(),
            nonce: String::new(),
            scope: crate::oauth::SCOPE.to_owned(),
            expires_in: 3600,
            omit_id_token: false,
            omit_id_token_on_refresh: false,
            kid: "k1".to_owned(),
            omit_scope_on_refresh: false,
            omit_refresh_token_on_refresh: false,
            discovery_status: 200,
            jwks_status: 200,
            token_error: None,
            token_redirect: None,
            hold: None,
            revoke_status: 200,
            discovery_issuer: None,
            discovery_token_endpoint: None,
        }
    }
}

/// What the fake has seen, and how it should answer.
#[derive(Debug, Default)]
pub struct Seen {
    /// The form fields of every token request, in order.
    pub token_requests: Vec<HashMap<String, String>>,
    /// Every refresh token presented for revocation, in order.
    pub revoked: Vec<String>,
    /// How many token sets have been issued.
    pub issued: u32,
    /// How many times the discovery document was asked for.
    pub discovery_hits: u32,
    /// How many times the signing keys were asked for.
    pub jwks_hits: u32,
    /// The refresh tokens the fake would still honor.
    pub valid_refresh_tokens: HashSet<String>,
    /// The PKCE challenge and redirect the last authorize URL carried.
    pub expected_exchange: Option<(String, String)>,
    /// The current behavior.
    pub behavior: Behavior,
}

#[derive(Clone)]
struct Shared {
    base: String,
    seen: Arc<Mutex<Seen>>,
    release: Arc<Notify>,
    arrived: Arc<Notify>,
}

/// A running fake. Dropping it stops the server.
pub struct Fake {
    /// The fake's origin, `http://127.0.0.1:<port>`: the issuer tests pin.
    pub base: String,
    seen: Arc<Mutex<Seen>>,
    release: Arc<Notify>,
    arrived: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl fmt::Debug for Fake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fake")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Fake {
    /// Starts a fake on a free loopback port.
    pub async fn start() -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let release = Arc::new(Notify::new());
        let arrived = Arc::new(Notify::new());
        let shared = Shared {
            base: base.clone(),
            seen: seen.clone(),
            release: release.clone(),
            arrived: arrived.clone(),
        };
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/.well-known/jwks.json", get(jwks))
            .route("/api/accounts/oauth/token", post(token))
            .route("/api/accounts/oauth/revoke", post(revoke))
            .with_state(shared);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Fake {
            base,
            seen,
            release,
            arrived,
            task,
        }
    }

    /// The configuration that pins this fake as the issuer.
    pub fn config(&self) -> crate::oauth::OAuthConfig {
        crate::oauth::OAuthConfig {
            issuer: self.base.clone(),
            resource: "https://api.example.test/v1".to_owned(),
            agent_name: "Runvil".to_owned(),
        }
    }

    /// Changes how the fake answers.
    pub fn set(&self, change: impl FnOnce(&mut Behavior)) {
        change(&mut self.seen.lock().expect("lock").behavior);
    }

    /// Reads what the fake has seen.
    pub fn seen<T>(&self, read: impl FnOnce(&Seen) -> T) -> T {
        read(&self.seen.lock().expect("lock"))
    }

    /// The `grant_type` of every token request, in order.
    pub fn grants(&self) -> Vec<String> {
        self.seen(|seen| {
            seen.token_requests
                .iter()
                .map(|fields| fields.get("grant_type").cloned().unwrap_or_default())
                .collect()
        })
    }

    /// Waits until a held refresh request has reached the fake.
    pub async fn refresh_arrived(&self) {
        self.arrived.notified().await;
    }

    /// Waits until a held code exchange has reached the fake.
    pub async fn exchange_arrived(&self) {
        self.arrived.notified().await;
    }

    /// Waits until a held revocation has reached the fake.
    pub async fn revoke_arrived(&self) {
        self.arrived.notified().await;
    }

    /// Makes `token` a refresh token the fake honors, as if it had issued
    /// it in an earlier run.
    pub fn accept_refresh_token(&self, token: &str) {
        self.seen
            .lock()
            .expect("lock")
            .valid_refresh_tokens
            .insert(token.to_owned());
    }

    /// Tells the fake what the authorize URL carried, which the real
    /// service remembers and checks at the code exchange.
    pub fn expect_exchange(&self, code_challenge: &str, redirect_uri: &str) {
        self.seen.lock().expect("lock").expected_exchange =
            Some((code_challenge.to_owned(), redirect_uri.to_owned()));
    }

    /// Lets every held request finish.
    pub fn release(&self) {
        self.set(|behavior| behavior.hold = None);
        self.release.notify_waiters();
    }
}

async fn discovery(State(shared): State<Shared>) -> Response {
    let behavior = {
        let mut seen = shared.seen.lock().expect("lock");
        seen.discovery_hits += 1;
        seen.behavior.clone()
    };
    if behavior.discovery_status != 200 {
        return StatusCode::from_u16(behavior.discovery_status)
            .expect("a status")
            .into_response();
    }
    let base = &shared.base;
    Json(json!({
        "issuer": behavior.discovery_issuer.unwrap_or_else(|| base.clone()),
        "authorization_endpoint": format!("{base}/api/accounts/authorize"),
        "token_endpoint": behavior
            .discovery_token_endpoint
            .unwrap_or_else(|| format!("{base}/api/accounts/oauth/token")),
        "revocation_endpoint": format!("{base}/api/accounts/oauth/revoke"),
        "jwks_uri": format!("{base}/.well-known/jwks.json"),
    }))
    .into_response()
}

async fn jwks(State(shared): State<Shared>) -> Response {
    let (status, kid) = {
        let mut seen = shared.seen.lock().expect("lock");
        seen.jwks_hits += 1;
        (seen.behavior.jwks_status, seen.behavior.kid.clone())
    };
    if status != 200 {
        return StatusCode::from_u16(status)
            .expect("a status")
            .into_response();
    }
    Json(jwks_document(&kid)).into_response()
}

/// A 400 with an OAuth error, as the real token endpoint answers.
fn refuse(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

/// Checks a token request the way the real service does, before anything is
/// issued: the documented fields, the PKCE verifier against the challenge
/// the authorize URL carried, and a refresh token that is still valid.
fn check_token_request(
    seen: &Seen,
    fields: &HashMap<String, String>,
    refreshing: bool,
) -> Option<Response> {
    let field = |name: &str| fields.get(name).map_or("", String::as_str);
    if field("client_id").is_empty() || field("resource").is_empty() {
        return Some(refuse("invalid_request"));
    }
    if refreshing {
        return (!seen.valid_refresh_tokens.contains(field("refresh_token")))
            .then(|| refuse("invalid_grant"));
    }
    if field("code").is_empty() {
        return Some(refuse("invalid_request"));
    }
    if let Some((challenge, redirect_uri)) = &seen.expected_exchange {
        let presented = URL_SAFE_NO_PAD.encode(Sha256::digest(field("code_verifier").as_bytes()));
        if presented != *challenge || field("redirect_uri") != redirect_uri {
            return Some(refuse("invalid_grant"));
        }
    }
    None
}

async fn token(
    State(shared): State<Shared>,
    Form(fields): Form<HashMap<String, String>>,
) -> Response {
    let refreshing = fields.get("grant_type").map(String::as_str) == Some("refresh_token");
    let (behavior, refusal) = {
        let mut seen = shared.seen.lock().expect("lock");
        seen.token_requests.push(fields.clone());
        let refusal = check_token_request(&seen, &fields, refreshing);
        (seen.behavior.clone(), refusal)
    };
    if let Some(location) = behavior.token_redirect {
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response();
    }
    let kind = if refreshing {
        Held::Refresh
    } else {
        Held::Exchange
    };
    if behavior.hold == Some(kind) {
        let released = shared.release.notified();
        shared.arrived.notify_one();
        released.await;
    }
    if let Some((status, error)) = behavior.token_error {
        let status = StatusCode::from_u16(status).expect("a status");
        return (
            status,
            Json(json!({ "error": error, "error_description": "secret detail" })),
        )
            .into_response();
    }
    if let Some(refusal) = refusal {
        return refusal;
    }
    let rotating = !(refreshing && behavior.omit_refresh_token_on_refresh);
    let number = {
        let mut seen = shared.seen.lock().expect("lock");
        seen.issued += 1;
        let number = seen.issued;
        if rotating {
            // A refresh token is good for one use: rotation retires the old one.
            if let Some(spent) = fields.get("refresh_token") {
                seen.valid_refresh_tokens.remove(spent);
            }
            seen.valid_refresh_tokens.insert(format!("rt-{number}"));
        }
        number
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let access = format!("at-{number}");
    let mut reply = json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": behavior.expires_in,
        "earliest_refresh_at": now + behavior.expires_in.saturating_sub(360),
    });
    if rotating {
        reply["refresh_token"] = json!(format!("rt-{number}"));
    }
    if !(refreshing && behavior.omit_scope_on_refresh) {
        reply["scope"] = json!(behavior.scope);
    }
    let wants_id_token = if refreshing {
        !behavior.omit_id_token_on_refresh
    } else {
        !behavior.omit_id_token
    };
    if wants_id_token {
        let mut claims = json!({
            "iss": shared.base,
            "aud": [fields.get("client_id").cloned().unwrap_or_default()],
            "sub": behavior.subject,
            "email": "a@example.test",
            "exp": now + 86_400,
            "at_hash": at_hash(&access),
        });
        if !refreshing {
            claims["nonce"] = json!(behavior.nonce);
        }
        reply["id_token"] = json!(sign(
            &json!({ "alg": "RS256", "kid": behavior.kid }),
            &claims
        ));
    }
    Json(reply).into_response()
}

async fn revoke(
    State(shared): State<Shared>,
    Form(fields): Form<HashMap<String, String>>,
) -> Response {
    let field = |name: &str| fields.get(name).map_or("", String::as_str);
    let documented = !field("token").is_empty()
        && field("token_type_hint") == "refresh_token"
        && !field("client_id").is_empty();
    let behavior = {
        let mut seen = shared.seen.lock().expect("lock");
        seen.revoked.push(field("token").to_owned());
        seen.behavior.clone()
    };
    if behavior.hold == Some(Held::Revoke) {
        let released = shared.release.notified();
        shared.arrived.notify_one();
        released.await;
    }
    if !documented {
        return refuse("invalid_request");
    }
    if behavior.revoke_status == 200 {
        shared
            .seen
            .lock()
            .expect("lock")
            .valid_refresh_tokens
            .remove(field("token"));
    }
    StatusCode::from_u16(behavior.revoke_status)
        .expect("a status")
        .into_response()
}
