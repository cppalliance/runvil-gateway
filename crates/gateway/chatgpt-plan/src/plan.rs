//! The broker: the one owner of a ChatGPT sign-in.
//!
//! [`ChatGptPlan`] is the single place the credential changes. It serializes
//! every change behind one lock and one generation counter, and it applies
//! one rule to every token the issuer hands it: the token is either adopted
//! into the saved record or revoked. It is never just dropped.
//!
//! Three properties shape the code:
//!
//! - **The broker owns every in-flight operation.** A refresh, a code
//!   exchange and a sign-out each run as a task the broker spawned, so a
//!   caller that goes away cannot abandon a reply that still has to be saved
//!   or revoked.
//! - **The generation check is part of the commit.** Sign-out and a new
//!   sign-in bump the generation, which lives under the same lock as the
//!   record. A result that arrives under an older generation is refused
//!   inside the critical section that would have saved it, and revoked.
//! - **An unknown outcome fails closed.** Before a refresh is sent, the
//!   record is saved as [`Session::Refreshing`]. A refresh whose reply
//!   never arrives, or a restart that finds [`Session::Refreshing`], leaves
//!   the stored refresh token possibly spent, so the sign-in becomes
//!   [`Session::NeedsAuth`] and nothing is retried. A refresh that never
//!   left (no connection) changes nothing.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use gateway_protocol::ProtocolError;
use gateway_protocol::upstream::BearerSource;

use crate::id_token::{self, IdClaims, IdTokenError, Jwks};
use crate::oauth::{OAuthClient, OAuthConfig, OAuthError, PLAN_SCOPE};
use crate::store::{Record, Revocation, Secret, Session, Status, Store, StoreError};

mod refresh;
mod sign_in;
mod sign_out;

pub use self::sign_in::Callback;
pub use self::sign_out::SignedOut;

/// How long before expiry an access token is refreshed.
const REFRESH_MARGIN_SECS: u64 = 120;

/// The time source, in Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Why the broker refused or failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// No one is signed in.
    #[error("not signed in to ChatGPT")]
    NotSignedIn,
    /// The sign-in can no longer be trusted; the user must sign in again.
    #[error("the ChatGPT sign-in must be repeated")]
    NeedsAuth,
    /// The account did not grant plan usage to this app.
    #[error("the ChatGPT account has not granted plan usage")]
    PlanNotGranted,
    /// The saved record cannot be read; it must be reset first.
    #[error("the saved ChatGPT sign-in is unreadable")]
    Unreadable,
    /// The callback matches no pending sign-in.
    #[error("no sign-in is waiting for this callback")]
    UnknownAttempt,
    /// The sign-in took too long, or was superseded by a sign-out or a
    /// newer sign-in.
    #[error("the sign-in expired or was superseded; start it again")]
    Superseded,
    /// The user or the issuer declined. Holds the OAuth error code.
    #[error("ChatGPT declined the sign-in ({0})")]
    Denied(String),
    /// The callback or the token reply lacks something it must carry.
    #[error("the sign-in reply is incomplete: {0}")]
    Incomplete(&'static str),
    /// The callback names a client other than this install's.
    #[error("the sign-in reply is for another registration")]
    ClientMismatch,
    /// The reply is for a different account than the one registered.
    #[error("signed in as a different account than this registration")]
    AccountMismatch,
    /// The ID token failed verification.
    #[error(transparent)]
    IdToken(#[from] IdTokenError),
    /// An OAuth call failed.
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    /// The record could not be saved or read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A broker operation stopped because system randomness failed or a spawned task ended.
    #[error("the ChatGPT sign-in operation could not complete ({0})")]
    Interrupted(&'static str),
}

impl PlanError {
    /// The stable identifier a client sees when this error stops a request.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            PlanError::NotSignedIn => "chatgpt_sign_in_required",
            PlanError::NeedsAuth | PlanError::Unreadable => "chatgpt_sign_in_expired",
            PlanError::PlanNotGranted => "chatgpt_plan_not_granted",
            _ => "chatgpt_sign_in_failed",
        }
    }
}

/// What the broker reports about itself. It never includes a token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact a status page shows"
)]
pub struct PlanStatus {
    /// Where the sign-in stands; `None` when no record has ever existed.
    pub status: Option<Status>,
    /// Whether the saved record is unreadable and must be reset.
    pub unreadable: bool,
    /// The signed-in account's email.
    pub email: Option<String>,
    /// Whether the account granted plan usage.
    pub plan_granted: bool,
    /// The last sign-out's remote outcome.
    pub revocation: Option<Revocation>,
    /// Whether a refresh token the broker did not keep could not be
    /// confirmed revoked, and so may still be live at the issuer.
    pub cleanup_unconfirmed: bool,
    /// Whether a sign-in is waiting for its callback or still completing one.
    pub sign_in_pending: bool,
}

/// Snapshot of an active, plan-granted sign-in's identity and generation.
///
/// Captured before an out-of-lock network call and checked before committing
/// its side effects, so a sign-out, reset, or account switch in between
/// aborts the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFence {
    generation: u64,
    setup_epoch: u64,
    subject: Option<String>,
}

struct Access {
    token: Secret,
    expires_at: u64,
}

#[derive(Default)]
struct State {
    record: Option<Record>,
    unreadable: bool,
    unconfirmed_cleanup: bool,
    access: Option<Access>,
    attempt: Option<sign_in::Attempt>,
    /// The serial of the claimed attempt that is still completing.
    completing: Option<u64>,
    /// Counts sign-ins started. An attempt that is not the latest one
    /// started may not activate, even when its exchange was already sent.
    attempts_started: u64,
    /// Bumped by sign-out, reset and every completed sign-in. A result that
    /// started under an older value is stale. It lives under the same lock
    /// as the record, so the fence and the change it fences are one step.
    generation: u64,
    /// Tracks each token-bearing retirement across tokenless repeats.
    next_retirement_id: u64,
    pending_retirements: std::collections::HashSet<u64>,
    signed_out_retirement: Option<u64>,
    /// Bumped when plan model bindings are explicitly forgotten, superseding
    /// any in-flight `models/enable` without disturbing OAuth credentials.
    setup_epoch: u64,
}

struct Inner {
    store: Store,
    oauth: OAuthClient,
    clock: Clock,
    state: Mutex<State>,
    /// Held for the whole of one refresh, so concurrent callers share it.
    refresh_gate: tokio::sync::Mutex<()>,
    jwks: tokio::sync::Mutex<Option<Jwks>>,
}

/// The broker. Cloning it shares the one sign-in.
#[derive(Clone)]
pub struct ChatGptPlan {
    inner: Arc<Inner>,
}

impl fmt::Debug for ChatGptPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGptPlan").finish_non_exhaustive()
    }
}

impl ChatGptPlan {
    /// Opens the sign-in saved in the profile directory `dir`, signing in
    /// with `config`.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the private store directory or its lock
    /// cannot be opened.
    pub fn open(dir: &Path, config: OAuthConfig) -> Result<ChatGptPlan, StoreError> {
        let clock: Clock = Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
        ChatGptPlan::open_with_clock(dir, config, clock)
    }

    /// [`ChatGptPlan::open`] with an injected clock.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the private store directory or its lock
    /// cannot be opened.
    pub fn open_with_clock(
        dir: &Path,
        config: OAuthConfig,
        clock: Clock,
    ) -> Result<ChatGptPlan, StoreError> {
        let store = Store::open(dir)?;
        let mut state = State::default();
        match store.load() {
            Ok(record) => {
                state.unconfirmed_cleanup = record
                    .as_ref()
                    .is_some_and(|record| record.unconfirmed_cleanup);
                state.record = record;
            }
            Err(_) => state.unreadable = true,
        }
        // A refresh was in flight when the last process ended: its outcome
        // is unknown, so the stored refresh token may be spent.
        if let Some(record) = state.record.as_mut()
            && let Session::Refreshing(creds) = &record.session
        {
            record.session = Session::NeedsAuth {
                refresh_token: Some(creds.refresh_token.clone()),
            };
            tracing::warn!("a ChatGPT token refresh was interrupted; sign in again");
            if store.save(record).is_err() {
                state.unreadable = true;
            }
        }
        Ok(ChatGptPlan {
            inner: Arc::new(Inner {
                store,
                oauth: OAuthClient::new(config),
                clock,
                state: Mutex::new(state),
                refresh_gate: tokio::sync::Mutex::new(()),
                jwks: tokio::sync::Mutex::new(None),
            }),
        })
    }

    /// What the broker reports about the sign-in. Never a token.
    #[must_use]
    pub fn status(&self) -> PlanStatus {
        let state = self.inner.lock();
        let record = state.record.as_ref();
        PlanStatus {
            status: record.map(|record| record.session.status()),
            unreadable: state.unreadable,
            email: record.and_then(|record| record.email.clone()),
            plan_granted: record.is_some_and(grants_plan),
            revocation: record.and_then(|record| record.session.revocation()),
            cleanup_unconfirmed: state.unconfirmed_cleanup,
            sign_in_pending: self.inner.sign_in_pending(&state),
        }
    }

    /// The bearer for the next plan request, refreshing first when the
    /// cached one is missing or about to expire.
    ///
    /// # Errors
    /// See [`PlanError`]. Nothing is returned unless the sign-in is active
    /// and plan usage was granted.
    pub async fn access_token(&self) -> Result<Secret, PlanError> {
        if let Some(token) = self.inner.cached_access()? {
            return Ok(token);
        }
        let inner = self.inner.clone();
        tokio::spawn(async move { inner.refresh().await })
            .await
            .map_err(|_| PlanError::Interrupted("token refresh task stopped"))?
    }

    /// Snapshots the active, plan-granted session's generation and account
    /// subject, or returns the broker refusal when no usable plan session is
    /// present.
    ///
    /// # Errors
    /// Returns [`PlanError`] when the store is unreadable, nobody is signed
    /// in, re-authentication is required, or plan usage was not granted.
    pub fn current_fence(&self) -> Result<SessionFence, PlanError> {
        let state = self.inner.lock();
        let _ = self.inner.usable(&state)?;
        let record = state.record.as_ref().ok_or(PlanError::NotSignedIn)?;
        Ok(SessionFence {
            generation: state.generation,
            setup_epoch: state.setup_epoch,
            subject: record.subject.clone(),
        })
    }

    /// Whether `fence` still matches the live, plan-granted session and setup
    /// epoch.
    #[must_use]
    pub fn verify_fence(&self, fence: &SessionFence) -> bool {
        let state = self.inner.lock();
        Inner::fence_matches(&state, fence)
    }

    /// Advances the model-setup epoch without signing out, so any in-flight
    /// model-enable operation started earlier cannot commit stale bindings.
    pub fn supersede_model_setup(&self) {
        let mut state = self.inner.lock();
        state.setup_epoch += 1;
    }

    /// Runs `commit` under the broker's state lock if and only if `fence`
    /// still matches the live session and setup epoch, serializing the side
    /// effect against sign-out, forget, reset, sign-in adoption, and
    /// [`ChatGptPlan::supersede_model_setup`].
    ///
    /// # Errors
    /// Returns `on_stale()` without running `commit` when `fence` is stale, or
    /// propagates the error returned by `commit`.
    pub fn commit_fenced<T, E, F>(
        &self,
        fence: &SessionFence,
        on_stale: impl FnOnce() -> E,
        commit: F,
    ) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        let state = self.inner.lock();
        if !Inner::fence_matches(&state, fence) {
            return Err(on_stale());
        }
        commit()
    }

    /// Drops `rejected` from the in-memory access-token cache if it is still
    /// the cached token, so the next [`ChatGptPlan::access_token`] refreshes.
    pub fn reject_access(&self, rejected: &Secret) {
        let mut state = self.inner.lock();
        if state
            .access
            .as_ref()
            .is_some_and(|access| access.token.expose() == rejected.expose())
        {
            state.access = None;
        }
    }
}

fn grants_plan(record: &Record) -> bool {
    record
        .session
        .scopes()
        .iter()
        .any(|scope| scope == PLAN_SCOPE)
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-change; the state is still the
        // last committed one, because every change is a whole assignment.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// Revokes a refresh token the broker will not keep. When the issuer
    /// does not confirm, the token is still not kept; what is kept is the
    /// fact that it may be live, which the status reports.
    async fn dispose(&self, client_id: &str, refresh_token: Option<&Secret>) {
        let Some(token) = refresh_token else { return };
        let revoked = self.oauth.revoke(client_id, token).await;
        tracing::info!(revoked, "revoked a ChatGPT refresh token that was not kept");
        if revoked {
            return;
        }
        self.note_unconfirmed_cleanup();
    }

    pub(super) fn note_unconfirmed_cleanup(&self) {
        let mut state = self.lock();
        state.unconfirmed_cleanup = true;
        if state.record.is_none()
            && !state.unreadable
            && let Some(agent_host_id) = sign_in::new_agent_host_id()
        {
            let mut warning = Record::new(agent_host_id);
            warning.unconfirmed_cleanup = true;
            if self.store.save(&warning).is_ok() {
                state.record = Some(warning);
            }
            return;
        }
        if let Some(record) = state
            .record
            .as_mut()
            .filter(|record| !record.unconfirmed_cleanup)
        {
            record.unconfirmed_cleanup = true;
            // Best effort: the flag is true in this process either way.
            let _ = self.store.save(record);
        }
    }

    /// Verifies an ID token, refetching the issuer's keys once when the
    /// token names a key the cache does not hold.
    async fn verify(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
        access_token: &str,
    ) -> Result<IdClaims, PlanError> {
        let config = self.oauth.config();
        let mut cache = self.jwks.lock().await;
        for refetch in [false, true] {
            if refetch || cache.is_none() {
                *cache = Some(self.oauth.jwks().await?);
            }
            let jwks = cache.clone().unwrap_or_default();
            let expected = id_token::Expected {
                issuer: &config.issuer,
                client_id,
                nonce,
                access_token,
                now: self.now(),
            };
            match id_token::verify(token, &jwks, &expected) {
                Err(IdTokenError::UnknownKey) if !refetch => {}
                outcome => return Ok(outcome?),
            }
        }
        Err(IdTokenError::UnknownKey.into())
    }

    fn fence_matches(state: &State, fence: &SessionFence) -> bool {
        !state.unreadable
            && state.generation == fence.generation
            && state.setup_epoch == fence.setup_epoch
            && state.record.as_ref().is_some_and(|record| {
                matches!(record.session, Session::Active(_) | Session::Refreshing(_))
                    && grants_plan(record)
                    && record.subject == fence.subject
            })
    }
}

#[async_trait]
impl BearerSource for ChatGptPlan {
    async fn bearer(&self) -> Result<gateway_config::Secret, ProtocolError> {
        match self.access_token().await {
            Ok(token) => Ok(gateway_config::Secret::new(token.expose().to_owned())),
            // The issuer could not be reached, or is failing: that is an
            // upstream fault, and the sign-in itself is still good.
            Err(PlanError::OAuth(error @ OAuthError::Connect)) => {
                Err(ProtocolError::connect(error))
            }
            Err(PlanError::OAuth(error)) => Err(ProtocolError::transport(error)),
            Err(error) => Err(ProtocolError::CredentialUnavailable { code: error.code() }),
        }
    }

    /// The backend refused this access token: stop handing it out, so the
    /// next request refreshes.
    async fn rejected(&self, rejected: &gateway_config::Secret) {
        let mut state = self.inner.lock();
        if state
            .access
            .as_ref()
            .is_some_and(|access| access.token.expose() == rejected.expose())
        {
            state.access = None;
        }
    }
}

#[cfg(test)]
#[path = "plan-tests.rs"]
mod tests;
