//! The app's hold on a ChatGPT sign-in: the two proofs minted at each start,
//! the broker that owns the sign-in once the profile directory is known, and
//! the one upstream that spends a signed-in user's plan.
//!
//! A proof is never a credential by itself. A caller presents one in its own
//! header on top of its ordinary authentication, and a repeated header
//! counts as none. The plan proof marks Runvil's own client; the owner proof
//! marks a caller that may manage what holds the plan's credential.
//!
//! OpenAI's Sign in with ChatGPT Terms,
//! <https://openai.com/policies/sign-in-with-chatgpt-terms/>, section 3.1,
//! restrict a plan to the application its user connected and forbid
//! offering it as general-purpose API access to other tools. The plan proof
//! is how this gateway keeps to that.
//!
//! A plan is spent in one place. The routing table's entry for a plan-backed
//! model holds [`Unresolved`], which refuses. The upstream that sends the
//! plan's credential lives here, goes to a compiled destination, and is
//! handed out only by [`ChatGpt::authorize_plan`].

use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::http::HeaderMap;
use gateway_chatgpt_plan::oauth::OAuthConfig;
use gateway_chatgpt_plan::plan::{ChatGptPlan, PlanStatus, SessionFence};
use gateway_chatgpt_plan::store::Status;
use gateway_config::Secret;
use tokio::task::AbortHandle;

use crate::error::GatewayError;
use crate::upstream::{BearerSource, ResponsesUpstream, StreamedChunks, Upstream};
use crate::wire::{ChatRequest, ChatResponse, RequestPolicy};

#[path = "chatgpt-models.rs"]
mod models;

pub(crate) use self::models::PlanModelEntry;

/// The header carrying the plan proof.
pub(crate) const PLAN_PROOF_HEADER: &str = "x-runvil-plan-proof";
/// The header carrying the owner proof.
pub(crate) const OWNER_PROOF_HEADER: &str = "x-runvil-owner-proof";

/// OpenAI's API: the only place a ChatGPT plan's credential is sent. It is
/// a compiled constant, so no configuration can name another destination.
const PLAN_API_BASE: &str = "https://api.openai.com/v1";

/// Set by a development run to sign in under another name, so a developer's
/// test sign-in is not listed as "Runvil" on their own account. It changes
/// only the name OpenAI shows the user. Where a credential goes is fixed.
const AGENT_NAME_ENV: &str = "RUNVIL_CHATGPT_AGENT_NAME";

/// Why a caller reached a plan-backed model's catalog entry instead of the
/// authorized upstream.
const UNRESOLVED: gateway_protocol::ProtocolError =
    gateway_protocol::ProtocolError::CredentialUnavailable {
        code: "chatgpt_plan_runvil_only",
    };

/// What this boot can do with a sign-in, decided once at start.
enum Broker {
    /// The broker is open on the profile directory.
    Open(ChatGptPlan),
    /// The gateway listens beyond loopback: it holds no sign-in and serves
    /// no plan-backed model. Fixed for the life of the process, whatever a
    /// later configuration says.
    NotLoopback,
    /// The broker could not be opened; the code and one-sentence reason.
    Unavailable { code: &'static str, reason: String },
}

struct ChatGptInner {
    plan_proof: Secret,
    owner_proof: Secret,
    broker: Arc<OnceLock<Broker>>,
    api_base: Arc<str>,
    models_client: Option<reqwest::Client>,
    upstream: Arc<dyn Upstream>,
    listener: tokio::sync::Mutex<Option<AbortHandle>>,
}

/// The proofs of this boot, the broker once opened, and the plan upstream.
#[derive(Clone)]
pub(crate) struct ChatGpt {
    inner: Arc<ChatGptInner>,
}

impl fmt::Debug for ChatGpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGpt").finish_non_exhaustive()
    }
}

impl ChatGpt {
    /// Mints this boot's proofs. The broker is not open yet.
    pub(crate) fn new() -> ChatGpt {
        ChatGpt::sending_to(PLAN_API_BASE)
    }

    /// A handle whose plan upstream sends to `api_base`, so a test can
    /// watch what a plan request carries. Production has one destination.
    #[cfg(test)]
    pub(crate) fn with_api_base(api_base: &str) -> ChatGpt {
        ChatGpt::sending_to_with_client(api_base, models::models_http_client())
    }

    #[cfg(test)]
    pub(crate) fn without_models_client(api_base: &str) -> ChatGpt {
        ChatGpt::sending_to_with_client(api_base, None)
    }

    fn sending_to(api_base: &str) -> ChatGpt {
        ChatGpt::sending_to_with_client(api_base, models::models_http_client())
    }

    fn sending_to_with_client(api_base: &str, models_client: Option<reqwest::Client>) -> ChatGpt {
        let broker = Arc::new(OnceLock::new());
        let bearer = PlanBearer {
            broker: Arc::clone(&broker),
        };
        ChatGpt {
            inner: Arc::new(ChatGptInner {
                plan_proof: mint(),
                owner_proof: mint(),
                broker,
                api_base: Arc::from(api_base),
                models_client,
                upstream: Arc::new(ResponsesUpstream::new(
                    api_base,
                    Arc::new(bearer),
                    RequestPolicy::Plan,
                )),
                listener: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// The task serving the current sign-in's callback listener, if any.
    ///
    /// A sign-in holds this lock from before it binds its listener until it
    /// has put its own task here and aborted the one before, so two
    /// sign-ins cannot interleave and a replaced sign-in keeps no port.
    pub(crate) fn listener(&self) -> &tokio::sync::Mutex<Option<AbortHandle>> {
        &self.inner.listener
    }

    /// The plan proof, for the private file.
    pub(crate) fn plan_proof(&self) -> &str {
        self.inner.plan_proof.expose()
    }

    /// The owner proof, for the private file.
    pub(crate) fn owner_proof(&self) -> &str {
        self.inner.owner_proof.expose()
    }

    /// Whether the request presents this boot's plan proof, exactly once.
    fn plan_proof_presented(&self, headers: &HeaderMap) -> bool {
        presented(headers, PLAN_PROOF_HEADER, &self.inner.plan_proof)
    }

    /// Whether the request presents this boot's owner proof, exactly once.
    pub(crate) fn owner_proof_presented(&self, headers: &HeaderMap) -> bool {
        presented(headers, OWNER_PROOF_HEADER, &self.inner.owner_proof)
    }

    /// Decides, once, what this boot can do with a sign-in.
    ///
    /// `bound` is the address the listener actually holds, not the
    /// configured one. Only `127.0.0.0/8` and `::1` qualify: a gateway that
    /// listens anywhere else holds no sign-in and serves no plan-backed
    /// model, so the credential is never reachable from another machine.
    /// Otherwise the broker opens on `profile_dir`, and a failure is kept
    /// as the reason sign-in is unavailable. The gateway serves everything
    /// else either way.
    pub(crate) fn start(&self, bound: SocketAddr, profile_dir: Option<&Path>) {
        if !bound.ip().is_loopback() {
            let _ = self.inner.broker.set(Broker::NotLoopback);
            return;
        }
        match profile_dir {
            Some(profile_dir) => self.open_on(profile_dir, oauth_config()),
            None => self.unavailable_coded(
                "no_profile_dir",
                "no user profile directory was found to keep a sign-in in",
            ),
        }
    }

    /// Opens the broker against the issuer `config` names. Production
    /// passes OpenAI's; a test passes a fake's.
    pub(crate) fn open_on(&self, profile_dir: &Path, config: OAuthConfig) {
        let opened = match ChatGptPlan::open(profile_dir, config) {
            Ok(plan) => Broker::Open(plan),
            Err(error) => {
                tracing::warn!("ChatGPT sign-in is unavailable: {error}");
                Broker::Unavailable {
                    code: "storage_unavailable",
                    reason: error.to_string(),
                }
            }
        };
        let _ = self.inner.broker.set(opened);
    }

    /// Records that the broker cannot be opened, with the reason.
    #[cfg(test)]
    pub(crate) fn unavailable(&self, reason: &str) {
        self.unavailable_coded("storage_unavailable", reason);
    }

    fn unavailable_coded(&self, code: &'static str, reason: &str) {
        let _ = self.inner.broker.set(Broker::Unavailable {
            code,
            reason: reason.to_owned(),
        });
    }

    /// Stable `(code, reason)` when the broker is not open, or `None` when it is.
    pub(crate) fn unavailable_info(&self) -> Option<(&'static str, String)> {
        let (code, reason) = match self.inner.broker.get() {
            Some(Broker::Open(_)) => return None,
            Some(Broker::NotLoopback) => (
                "not_loopback",
                "ChatGPT sign-in needs a gateway that listens on loopback only",
            ),
            Some(Broker::Unavailable { code, reason }) => (*code, reason.as_str()),
            None => ("starting", "the gateway has not finished starting"),
        };
        Some((code, reason.to_owned()))
    }

    /// The broker.
    ///
    /// # Errors
    /// [`GatewayError::ChatGptUnavailable`] with the reason when the broker
    /// is not open.
    pub(crate) fn broker(&self) -> Result<&ChatGptPlan, GatewayError> {
        match self.inner.broker.get() {
            Some(Broker::Open(plan)) => Ok(plan),
            _ => Err(GatewayError::ChatGptUnavailable(
                self.unavailable_info()
                    .map_or_else(String::new, |(_, reason)| reason),
            )),
        }
    }

    /// Fetches `{api_base}/models` using the signed-in plan bearer, returning
    /// the session fence and closed model projection.
    pub(crate) async fn fetch_plan_models(
        &self,
    ) -> Result<(SessionFence, Vec<PlanModelEntry>), GatewayError> {
        let plan = self.broker()?;
        let client = self.inner.models_client.as_ref().ok_or_else(|| {
            GatewayError::ChatGptUnavailable("ChatGPT models HTTP client is unavailable".to_owned())
        })?;
        models::fetch(client, &self.inner.api_base, plan).await
    }

    /// Whether a plan-backed model may be listed for this request: the
    /// gateway listens on loopback only, the caller is Runvil's own client,
    /// and the broker's in-memory status is active with plan usage granted.
    pub(crate) fn plan_visible(&self, headers: &HeaderMap) -> bool {
        let Some(Broker::Open(plan)) = self.inner.broker.get() else {
            return false;
        };
        if !self.plan_proof_presented(headers) {
            return false;
        }
        plan_status_refusal(&plan.status()).is_none()
    }

    /// Authorizes one request for a plan-backed model and returns the
    /// upstream that spends the plan.
    ///
    /// # Errors
    /// [`GatewayError::ChatGptPlanLoopbackOnly`] on a gateway that listens
    /// beyond loopback, which wins over a missing proof;
    /// [`GatewayError::ChatGptPlanRunvilOnly`] without this boot's plan
    /// proof; [`GatewayError::ChatGptUnavailable`] when no sign-in can be
    /// held; or `403` with the sign-in refusal code when the broker's
    /// in-memory status is not active with plan usage granted.
    pub(crate) fn authorize_plan(
        &self,
        headers: &HeaderMap,
    ) -> Result<Arc<dyn Upstream>, GatewayError> {
        if matches!(self.inner.broker.get(), Some(Broker::NotLoopback)) {
            return Err(GatewayError::ChatGptPlanLoopbackOnly);
        }
        if !self.plan_proof_presented(headers) {
            return Err(GatewayError::ChatGptPlanRunvilOnly);
        }
        let plan = self.broker()?;
        if let Some(code) = plan_status_refusal(&plan.status()) {
            return Err(gateway_protocol::ProtocolError::CredentialUnavailable { code }.into());
        }
        Ok(Arc::clone(&self.inner.upstream))
    }
}

/// Refusal code for `plan_visible` / `authorize_plan` from in-memory broker
/// status alone (zero network).
fn plan_status_refusal(status: &PlanStatus) -> Option<&'static str> {
    if status.unreadable {
        return Some("chatgpt_sign_in_expired");
    }
    match status.status {
        None | Some(Status::SignedOut) => Some("chatgpt_sign_in_required"),
        Some(Status::NeedsAuth) => Some("chatgpt_sign_in_expired"),
        Some(Status::Active) if !status.plan_granted => Some("chatgpt_plan_not_granted"),
        Some(Status::Active) => None,
    }
}

/// OpenAI's sign-in, under the name [`AGENT_NAME_ENV`] gives when it holds a
/// usable one.
fn oauth_config() -> OAuthConfig {
    oauth_config_named(std::env::var(AGENT_NAME_ENV).ok().as_deref())
}

/// OpenAI's sign-in, asking as `requested` when that is a usable name and
/// as "Runvil" otherwise.
fn oauth_config_named(requested: Option<&str>) -> OAuthConfig {
    let mut config = OAuthConfig::openai();
    if let Some(raw) = requested {
        if let Some(name) = agent_name(raw) {
            tracing::info!("ChatGPT sign-in will ask as {name:?}, from {AGENT_NAME_ENV}");
            config.agent_name = name;
        } else {
            tracing::warn!(
                "{AGENT_NAME_ENV} is not a usable name; signing in as {:?}",
                config.agent_name
            );
        }
    }
    config
}

/// A name fit to show on a consent page: 1 to 64 characters, each an ASCII
/// letter or digit, a space, or one of `-_.()`. Anything else is refused,
/// so the variable cannot put markup or a look-alike string there.
fn agent_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let usable = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " -_.()".contains(c));
    usable.then(|| name.to_owned())
}

/// The plan upstream's credential: the broker's current access token.
struct PlanBearer {
    broker: Arc<OnceLock<Broker>>,
}

impl fmt::Debug for PlanBearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanBearer").finish_non_exhaustive()
    }
}

#[async_trait]
impl BearerSource for PlanBearer {
    async fn bearer(&self) -> Result<Secret, gateway_protocol::ProtocolError> {
        match self.broker.get() {
            Some(Broker::Open(plan)) => plan.bearer().await,
            _ => Err(gateway_protocol::ProtocolError::CredentialUnavailable {
                code: "chatgpt_unavailable",
            }),
        }
    }

    async fn rejected(&self, rejected: &Secret) {
        if let Some(Broker::Open(plan)) = self.broker.get() {
            plan.rejected(rejected).await;
        }
    }
}

/// What the routing table holds as a plan-backed model's upstream. It
/// refuses every request, so a handler that reads a model out of the table
/// without [`ChatGpt::authorize_plan`] spends nothing.
#[derive(Debug)]
pub(crate) struct Unresolved;

#[async_trait]
impl Upstream for Unresolved {
    async fn send(
        &self,
        _req: ChatRequest,
        _upstream_model: &str,
    ) -> Result<ChatResponse, gateway_protocol::ProtocolError> {
        Err(UNRESOLVED)
    }

    async fn stream(
        &self,
        _req: ChatRequest,
        _upstream_model: &str,
    ) -> Result<StreamedChunks, gateway_protocol::ProtocolError> {
        Err(UNRESOLVED)
    }
}

/// A fresh proof: 32 bytes from the OS-seeded CSPRNG, as lowercase hex.
fn mint() -> Secret {
    use std::fmt::Write as _;

    use rand::Rng as _;

    let mut bytes = [0_u8; 32];
    rand::rng().fill(&mut bytes);
    let mut proof = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(proof, "{byte:02x}");
    }
    Secret::new(proof)
}

/// Whether `headers` holds exactly one `name` header whose value is `proof`.
fn presented(headers: &HeaderMap, name: &str, proof: &Secret) -> bool {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => crate::auth::secret_eq(value.as_bytes(), proof.expose().as_bytes()),
        _ => false,
    }
}

#[cfg(test)]
#[path = "chatgpt-tests.rs"]
mod tests;
