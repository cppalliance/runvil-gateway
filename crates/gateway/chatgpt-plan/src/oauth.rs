//! The OAuth calls of the Sign in with ChatGPT flow: discovery, the
//! authorize URL, the code exchange, refresh, revocation, and the JWKS.
//!
//! Every request goes to the pinned issuer and nowhere else. Discovery is
//! trusted only when its document names the pinned issuer and every
//! endpoint it lists has the issuer's origin; otherwise the documented
//! endpoints are used. No request follows a redirect, because a followed
//! redirect would carry a code or a refresh token to another server. Replies
//! are size-capped and time-bounded. An error keeps the OAuth error code
//! when it is a plain lowercase word and nothing else of the reply.
//!
//! Reference: OpenAI's "Registration and sign-in", <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>,
//! and "Token reference", <https://developers.openai.com/siwc/token-sharing-open-source/token-reference>.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::id_token::Jwks;
use crate::store::Secret;

/// The scopes every sign-in asks for.
pub const SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// The scope that lets the plan pay for model calls.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// The client id of a first sign-in, which registers this install.
const REGISTRATION_CLIENT_ID: &str = "dynamic_agent_client";

/// The largest reply read from the issuer.
const MAX_REPLY: usize = 256 * 1024;

/// Who the Gateway signs in with, and as what.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// The pinned issuer origin, without a trailing slash.
    pub issuer: String,
    /// The API the tokens are for.
    pub resource: String,
    /// The name the consent screen shows.
    pub agent_name: String,
}

impl OAuthConfig {
    /// OpenAI's production issuer and API, signing in as "Runvil".
    #[must_use]
    pub fn openai() -> OAuthConfig {
        OAuthConfig {
            issuer: "https://auth.openai.com".to_owned(),
            resource: "https://api.openai.com/v1".to_owned(),
            agent_name: "Runvil".to_owned(),
        }
    }
}

/// The issuer's endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Where the browser is sent to sign in.
    pub authorize: String,
    /// The code exchange and refresh endpoint.
    pub token: String,
    /// The revocation endpoint.
    pub revoke: String,
    /// The signing keys.
    pub jwks: String,
}

impl Endpoints {
    /// The endpoints OpenAI documents, under `issuer`.
    #[must_use]
    pub fn documented(issuer: &str) -> Endpoints {
        Endpoints {
            authorize: format!("{issuer}/api/accounts/authorize"),
            token: format!("{issuer}/api/accounts/oauth/token"),
            revoke: format!("{issuer}/api/accounts/oauth/revoke"),
            jwks: format!("{issuer}/.well-known/jwks.json"),
        }
    }

    /// The endpoints a discovery document lists, when the document names
    /// `issuer` and every endpoint has the issuer's origin.
    fn from_discovery(issuer: &str, document: &Value) -> Option<Endpoints> {
        if document.get("issuer").and_then(Value::as_str) != Some(issuer) {
            return None;
        }
        let origin = Url::parse(issuer).ok()?.origin();
        let endpoint = |name: &str| {
            let url = document.get(name)?.as_str()?;
            (Url::parse(url).ok()?.origin() == origin).then(|| url.to_owned())
        };
        Some(Endpoints {
            authorize: endpoint("authorization_endpoint")?,
            token: endpoint("token_endpoint")?,
            revoke: endpoint("revocation_endpoint")?,
            jwks: endpoint("jwks_uri")?,
        })
    }
}

/// A token response.
#[derive(Debug, Clone)]
pub struct TokenSet {
    /// The bearer for the API, valid for `expires_in` seconds.
    pub access_token: Secret,
    /// The next refresh token, when the reply rotates it.
    pub refresh_token: Option<Secret>,
    /// The ID token, when the reply carries one.
    pub id_token: Option<Secret>,
    /// The access token's lifetime in seconds.
    pub expires_in: u64,
    /// The granted scopes, when the reply names them. A reply that omits
    /// `scope` grants what was granted before (RFC 6749, section 6).
    pub scopes: Option<Vec<String>>,
}

impl TokenSet {
    fn from_reply(reply: &Value) -> Option<TokenSet> {
        let secret = |name: &str| {
            reply
                .get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| Secret::new(value.to_owned()))
        };
        Some(TokenSet {
            access_token: secret("access_token")?,
            refresh_token: secret("refresh_token"),
            id_token: secret("id_token"),
            expires_in: reply
                .get("expires_in")
                .and_then(Value::as_u64)
                .filter(|secs| *secs > 0)?,
            scopes: reply
                .get("scope")
                .and_then(Value::as_str)
                .map(|scope| scope.split_whitespace().map(str::to_owned).collect()),
        })
    }
}

/// Why an OAuth call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OAuthError {
    /// The request never left: nothing changed at the issuer.
    #[error("could not reach the ChatGPT sign-in service")]
    Connect,
    /// The request may have arrived, but no reply came back: the outcome
    /// is unknown.
    #[error("the ChatGPT sign-in service did not answer")]
    NoReply,
    /// The issuer refused. `code` is its OAuth error code when that is a
    /// plain lowercase word, else `unrecognized_error`.
    #[error("the ChatGPT sign-in service refused the request ({code})")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The OAuth error code.
        code: String,
    },
    /// A success reply that does not hold what it must.
    #[error("the ChatGPT sign-in service sent an unreadable reply")]
    Malformed,
}

impl OAuthError {
    /// Whether a refresh that failed this way can never succeed with the
    /// same refresh token.
    #[must_use]
    pub fn ends_the_session(&self) -> bool {
        const TERMINAL: [&str; 6] = [
            "invalid_grant",
            "invalid_refresh_token",
            "token_expired",
            "refresh_token_expired",
            "refresh_token_invalidated",
            "refresh_token_reused",
        ];
        matches!(self, OAuthError::Refused { code, .. } if TERMINAL.contains(&code.as_str()))
    }
}

/// What the authorize URL carries for one sign-in attempt.
#[derive(Debug, Clone, Copy)]
pub struct AuthorizeRequest<'a> {
    /// The issued client id, or `None` to register this install.
    pub client_id: Option<&'a str>,
    /// The install's `ext_agent_host_id`.
    pub agent_host_id: &'a str,
    /// The loopback callback this attempt listens on.
    pub redirect_uri: &'a str,
    /// The attempt's single-use state.
    pub state: &'a str,
    /// The attempt's nonce, echoed in the ID token.
    pub nonce: &'a str,
    /// The attempt's PKCE verifier.
    pub verifier: &'a str,
    /// The account's email, to preselect it on a later sign-in.
    pub login_hint: Option<&'a str>,
}

/// Connect timeout for OAuth requests to the pinned issuer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Whole-request timeout for OAuth requests to the pinned issuer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The OAuth client: one HTTP client, the pinned configuration, and the
/// endpoints once resolved.
#[derive(Debug)]
pub struct OAuthClient {
    config: OAuthConfig,
    /// Dedicated non-redirecting bounded HTTP client, or `None` if construction
    /// failed so every request fails closed as [`OAuthError::Connect`].
    http: Option<reqwest::Client>,
    endpoints: tokio::sync::OnceCell<Endpoints>,
}

impl OAuthClient {
    /// A client for `config`. It never follows redirects and bounds every
    /// request to 30 seconds. Construction failure leaves the client in a
    /// fail-closed unavailable state rather than falling back to a default
    /// redirect-following client or dropping the timeout bounds.
    #[must_use]
    pub fn new(config: OAuthConfig) -> OAuthClient {
        let http =
            gateway_protocol::http_util::hardened_client(CONNECT_TIMEOUT, Some(REQUEST_TIMEOUT));
        OAuthClient {
            config,
            http,
            endpoints: tokio::sync::OnceCell::new(),
        }
    }

    /// Test-only constructor modeling a failed hardened HTTP client build.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_http_client(config: OAuthConfig) -> OAuthClient {
        OAuthClient {
            config,
            http: None,
            endpoints: tokio::sync::OnceCell::new(),
        }
    }

    /// The pinned configuration.
    #[must_use]
    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// The issuer's endpoints: discovered once when the discovery document
    /// passes the pinning checks, the documented ones otherwise. A discovery
    /// that could not be fetched is not remembered, so the next call tries
    /// again.
    pub async fn endpoints(&self) -> Endpoints {
        let discovered = self
            .endpoints
            .get_or_try_init(|| async {
                let url = format!("{}/.well-known/openid-configuration", self.config.issuer);
                let document = self.get_json(&url).await?;
                Ok::<_, OAuthError>(
                    Endpoints::from_discovery(&self.config.issuer, &document).unwrap_or_else(
                        || {
                            tracing::warn!("ChatGPT sign-in discovery failed its pinning checks");
                            Endpoints::documented(&self.config.issuer)
                        },
                    ),
                )
            })
            .await;
        match discovered {
            Ok(endpoints) => endpoints.clone(),
            Err(_) => Endpoints::documented(&self.config.issuer),
        }
    }

    /// The URL the browser opens for `request`.
    ///
    /// # Errors
    /// [`OAuthError::Malformed`] when the authorize endpoint is not a URL.
    pub async fn authorize_url(
        &self,
        request: &AuthorizeRequest<'_>,
    ) -> Result<String, OAuthError> {
        let endpoints = self.endpoints().await;
        let mut url = Url::parse(&endpoints.authorize).map_err(|_| OAuthError::Malformed)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(request.verifier.as_bytes()));
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair(
                    "client_id",
                    request.client_id.unwrap_or(REGISTRATION_CLIENT_ID),
                )
                .append_pair("ext_agent_host_id", request.agent_host_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", request.redirect_uri)
                .append_pair("scope", SCOPE)
                .append_pair("resource", &self.config.resource)
                .append_pair("state", request.state)
                .append_pair("nonce", request.nonce)
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", &challenge);
            if request.client_id.is_none() {
                // OpenAI, "Registration and sign-in", section 1: "use your app's
                // actual name consistently as `agent_name_hint`".
                query.append_pair("agent_name_hint", &self.config.agent_name);
            } else if let Some(hint) = request.login_hint {
                query.append_pair("login_hint", hint);
            }
        }
        Ok(url.into())
    }

    /// Exchanges an authorization `code` for tokens.
    ///
    /// # Errors
    /// See [`OAuthError`].
    pub async fn exchange(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenSet, OAuthError> {
        let token = self.endpoints().await.token;
        self.token_request(
            &token,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", client_id),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
                ("resource", &self.config.resource),
            ],
        )
        .await
    }

    /// Trades `refresh_token` for a new token set.
    ///
    /// # Errors
    /// See [`OAuthError`]. [`OAuthError::NoReply`] means the refresh token
    /// may already be spent.
    pub async fn refresh(
        &self,
        client_id: &str,
        refresh_token: &Secret,
    ) -> Result<TokenSet, OAuthError> {
        let token = self.endpoints().await.token;
        self.token_request(
            &token,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token.expose()),
                ("resource", &self.config.resource),
            ],
        )
        .await
    }

    /// Revokes `refresh_token`. `true` only when the issuer answered 200:
    /// a timeout or any other reply is not a confirmation.
    pub async fn revoke(&self, client_id: &str, refresh_token: &Secret) -> bool {
        let Some(http) = self.http.as_ref() else {
            return false;
        };
        let revoke = self.endpoints().await.revoke;
        let fields = [
            ("token", refresh_token.expose()),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ];
        let request = post_form(http, &revoke, &fields).timeout(Duration::from_secs(10));
        matches!(request.send().await, Ok(reply) if reply.status() == reqwest::StatusCode::OK)
    }

    /// Fetches the issuer's signing keys.
    ///
    /// # Errors
    /// See [`OAuthError`].
    pub async fn jwks(&self) -> Result<Jwks, OAuthError> {
        let url = self.endpoints().await.jwks;
        Ok(Jwks::from_document(&self.get_json(&url).await?))
    }

    fn client(&self) -> Result<&reqwest::Client, OAuthError> {
        self.http.as_ref().ok_or(OAuthError::Connect)
    }

    async fn token_request(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> Result<TokenSet, OAuthError> {
        let reply = post_form(self.client()?, url, fields)
            .send()
            .await
            .map_err(classify)?;
        let reply = read_json(reply).await?;
        TokenSet::from_reply(&reply).ok_or(OAuthError::Malformed)
    }

    async fn get_json(&self, url: &str) -> Result<Value, OAuthError> {
        read_json(self.client()?.get(url).send().await.map_err(classify)?).await
    }
}

fn post_form(
    http: &reqwest::Client,
    url: &str,
    fields: &[(&str, &str)],
) -> reqwest::RequestBuilder {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    http.post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
}

/// A connect failure never reached the issuer; anything else may have.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the signature map_err expects"
)]
fn classify(error: reqwest::Error) -> OAuthError {
    if error.is_connect() {
        OAuthError::Connect
    } else {
        OAuthError::NoReply
    }
}

/// Reads a reply as JSON. A non-success status becomes
/// [`OAuthError::Refused`], keeping only a plain OAuth error code.
async fn read_json(reply: reqwest::Response) -> Result<Value, OAuthError> {
    let status = reply.status();
    let bytes = gateway_protocol::http_util::read_bytes_capped(reply, MAX_REPLY)
        .await
        .map_err(|_| OAuthError::NoReply)?;
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if !status.is_success() {
        let code = parsed
            .ok()
            .and_then(|body| body.get("error").and_then(Value::as_str).map(str::to_owned))
            .filter(|code| {
                (1..=40).contains(&code.len())
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            })
            .unwrap_or_else(|| "unrecognized_error".to_owned());
        return Err(OAuthError::Refused {
            status: status.as_u16(),
            code,
        });
    }
    parsed.map_err(|_| OAuthError::Malformed)
}

/// `bytes` of operating-system randomness, base64url-encoded, or `None`
/// when the system random source fails.
pub(crate) fn random_token(bytes: usize) -> Option<String> {
    let mut buffer = vec![0_u8; bytes];
    aws_lc_rs::rand::fill(&mut buffer).ok()?;
    Some(URL_SAFE_NO_PAD.encode(buffer))
}

#[cfg(test)]
#[path = "oauth-tests.rs"]
mod tests;
