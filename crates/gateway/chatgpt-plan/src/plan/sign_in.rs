//! Signing in: starting an attempt and completing it from the callback.
//!
//! One attempt is pending at a time; starting another cancels the one before
//! it, even when that one's code exchange has already been sent. The callback is matched by its `state`, compared without leaking
//! where it differs, and a callback that matches nothing consumes nothing.
//! A matching callback consumes the attempt whatever it carries, so a
//! `state` is good for one use. A denial sends no token request. On a first
//! sign-in the issued client id is saved before the code is exchanged, as
//! the issuer's documentation requires.
//!
//! Reference: OpenAI's "Registration and sign-in", sections 1 to 4,
//! <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>.

use std::fmt::Write as _;
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::{Access, ChatGptPlan, Inner, PlanError};
use crate::oauth::{AuthorizeRequest, SCOPE, TokenSet, random_token};
use crate::store::{Credentials, Record, Secret, Session};

/// How long a sign-in may take, from the authorize URL to its callback.
const ATTEMPT_LIFETIME_SECS: u64 = 600;

/// One pending sign-in.
pub(super) struct Attempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
    /// Whether this attempt registers the install (no client id yet).
    registering: bool,
    /// The generation the attempt started under.
    generation: u64,
    /// Which sign-in this is, counted from the broker's start.
    serial: u64,
    started_at: u64,
}

/// The callback's query, already parsed strictly: each parameter at most
/// once.
#[derive(Debug, Clone, Default)]
pub struct Callback {
    /// The `state` parameter.
    pub state: String,
    /// The authorization code, on success.
    pub code: Option<String>,
    /// The OAuth error, on refusal.
    pub error: Option<String>,
    /// The issued client id, which a first sign-in carries.
    pub client_id: Option<String>,
}

impl ChatGptPlan {
    /// Starts a sign-in whose callback will arrive at `redirect_uri`, and
    /// returns the URL to open in the browser. The first sign-in of an
    /// install creates its `ext_agent_host_id`.
    ///
    /// # Errors
    /// [`PlanError::Unreadable`] when the saved record must be reset first,
    /// and the store and OAuth errors of saving that id and building
    /// the URL.
    pub async fn begin_sign_in(&self, redirect_uri: &str) -> Result<String, PlanError> {
        let inner = &self.inner;
        let token = |bytes| {
            random_token(bytes).ok_or(PlanError::Interrupted("system randomness unavailable"))
        };
        let (state_token, nonce, verifier) = (token(32)?, token(32)?, token(48)?);
        let (agent_host_id, client_id, login_hint, generation) = {
            let mut state = inner.lock();
            if state.unreadable {
                return Err(PlanError::Unreadable);
            }
            if state.record.is_none() {
                let id = new_agent_host_id()
                    .ok_or(PlanError::Interrupted("system randomness unavailable"))?;
                let mut record = Record::new(id);
                record.unconfirmed_cleanup = state.unconfirmed_cleanup;
                inner.store.save(&record)?;
                state.record = Some(record);
            }
            let record = state
                .record
                .as_ref()
                .ok_or(PlanError::Interrupted("sign-in record missing after setup"))?;
            (
                record.agent_host_id.clone(),
                record.client_id.clone(),
                record.email.clone(),
                state.generation,
            )
        };
        // The URL is built before the attempt is installed, so a failure
        // here leaves nothing pending.
        let url = inner
            .oauth
            .authorize_url(&AuthorizeRequest {
                client_id: client_id.as_deref(),
                agent_host_id: &agent_host_id,
                redirect_uri,
                state: &state_token,
                nonce: &nonce,
                verifier: &verifier,
                login_hint: login_hint.as_deref(),
            })
            .await?;
        let mut state = inner.lock();
        // A sign-out or a completed sign-in since the record was read makes
        // what the URL was built from stale.
        if state.unreadable || state.generation != generation {
            return Err(PlanError::Superseded);
        }
        // A completion still open for an older attempt is no longer the
        // sign-in the status describes.
        state.completing = None;
        state.attempts_started += 1;
        state.attempt = Some(Attempt {
            state: state_token,
            nonce,
            verifier,
            redirect_uri: redirect_uri.to_owned(),
            registering: client_id.is_none(),
            generation,
            serial: state.attempts_started,
            started_at: inner.now(),
        });
        Ok(url)
    }

    /// Completes the pending sign-in from its callback: the exchange, the
    /// ID-token checks, and the activation.
    ///
    /// # Errors
    /// See [`PlanError`]. After any error the sign-in is not active, and a
    /// refresh token the issuer handed over has been revoked.
    pub async fn complete_sign_in(&self, callback: Callback) -> Result<(), PlanError> {
        let inner = self.inner.clone();
        // The broker owns the exchange: a callback connection that closes
        // early cannot abandon tokens that still need saving or revoking.
        tokio::spawn(async move { inner.complete(callback).await })
            .await
            .map_err(|_| PlanError::Interrupted("sign-in completion task stopped"))?
    }
}

/// Marks one claimed attempt, by its serial, as still completing. The
/// callback takes the attempt before the exchange is sent, so without this
/// mark the status would say that nothing is pending while a sign-in is
/// about to become active, and a client that polls would stop waiting.
///
/// The mark lasts until the attempt's own work is finished, the revocation
/// of a token it will not keep included, so the status a client reads once
/// nothing is pending is final for that attempt. A sign-out, a reset, or a
/// newer attempt clears the mark at once. Dropping this clears the mark
/// only while it is still this attempt's, so an older completion that
/// finishes late cannot clear a newer one's.
pub(super) struct Completing(pub(super) Arc<Inner>, pub(super) u64);

impl Drop for Completing {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        if state.completing == Some(self.1) {
            state.completing = None;
        }
    }
}

impl Inner {
    async fn complete(self: Arc<Inner>, callback: Callback) -> Result<(), PlanError> {
        // Claim: only the matching callback consumes the attempt.
        let (attempt, client_id) = {
            let mut state = self.lock();
            let matches = state
                .attempt
                .as_ref()
                .is_some_and(|attempt| same(&attempt.state, &callback.state));
            if !matches {
                return Err(PlanError::UnknownAttempt);
            }
            let attempt = state.attempt.take().ok_or(PlanError::UnknownAttempt)?;
            if self.expired(&attempt) {
                return Err(PlanError::Superseded);
            }
            if let Some(error) = &callback.error {
                return Err(PlanError::Denied(plain_code(error)));
            }
            let record = state.record.as_mut().ok_or(PlanError::Superseded)?;
            let client_id = if attempt.registering {
                let issued = callback
                    .client_id
                    .clone()
                    .filter(|id| !id.is_empty())
                    .ok_or(PlanError::Incomplete("no client id was issued"))?;
                // Saved before the exchange, and published only once saved.
                let mut registered = record.clone();
                registered.client_id = Some(issued.clone());
                self.store.save(&registered)?;
                *record = registered;
                issued
            } else {
                let known = record.client_id.clone().ok_or(PlanError::Superseded)?;
                if callback.client_id.as_ref().is_some_and(|id| *id != known) {
                    return Err(PlanError::ClientMismatch);
                }
                known
            };
            state.completing = Some(attempt.serial);
            (attempt, client_id)
        };
        let _completing = Completing(Arc::clone(&self), attempt.serial);
        let code = callback
            .code
            .as_deref()
            .filter(|code| !code.is_empty())
            .ok_or(PlanError::Incomplete("no authorization code"))?;

        let tokens = self
            .oauth
            .exchange(&client_id, code, &attempt.verifier, &attempt.redirect_uri)
            .await?;
        match self.adopt(&attempt, &client_id, &tokens).await {
            Ok(superseded) => {
                // The refresh token this sign-in replaced is no longer
                // saved, so it is revoked: a token is never just dropped.
                self.dispose(&client_id, superseded.as_ref()).await;
                Ok(())
            }
            Err(error) => {
                self.dispose(&client_id, tokens.refresh_token.as_ref())
                    .await;
                Err(error)
            }
        }
    }

    /// Verifies a code exchange's tokens and makes the sign-in active,
    /// returning the refresh token it replaced, if the record held one. On
    /// any error nothing was activated, and the caller revokes the tokens.
    async fn adopt(
        &self,
        attempt: &Attempt,
        client_id: &str,
        tokens: &TokenSet,
    ) -> Result<Option<Secret>, PlanError> {
        let id_token = tokens
            .id_token
            .as_ref()
            .ok_or(PlanError::Incomplete("no ID token"))?;
        let refresh_token = tokens
            .refresh_token
            .as_ref()
            .ok_or(PlanError::Incomplete("no refresh token"))?;
        let claims = self
            .verify(
                id_token.expose(),
                client_id,
                Some(&attempt.nonce),
                tokens.access_token.expose(),
            )
            .await?;

        // Activation: every check and the save are one critical section.
        let mut state = self.lock();
        // Superseded by a sign-out or a completed sign-in, by a newer
        // sign-in that has started, or by its own age.
        if state.generation != attempt.generation
            || state.attempts_started != attempt.serial
            || self.expired(attempt)
        {
            return Err(PlanError::Superseded);
        }
        let expires_at = self.now().saturating_add(tokens.expires_in);
        let revoke_settled = !state
            .signed_out_retirement
            .is_some_and(|id| state.pending_retirements.contains(&id));
        let record = state.record.as_mut().ok_or(PlanError::Superseded)?;
        if record
            .subject
            .as_ref()
            .is_some_and(|subject| *subject != claims.subject)
        {
            return Err(PlanError::AccountMismatch);
        }
        let before = record.clone();
        let carry_unconfirmed = before.unconfirmed_cleanup
            || (revoke_settled
                && matches!(
                    before.session,
                    Session::SignedOut {
                        revocation: Some(crate::store::Revocation::Unconfirmed),
                    }
                ));
        let superseded = record
            .session
            .refresh_token()
            .filter(|held| *held != refresh_token)
            .cloned();
        // A reply that names no scope granted what was asked for.
        let scopes = tokens
            .scopes
            .clone()
            .unwrap_or_else(|| SCOPE.split(' ').map(str::to_owned).collect());
        record.subject = Some(claims.subject);
        record.email = claims.email;
        record.unconfirmed_cleanup = carry_unconfirmed;
        record.session = Session::Active(Credentials {
            refresh_token: refresh_token.clone(),
            scopes,
        });
        if let Err(error) = self.store.save(record) {
            *record = before;
            return Err(error.into());
        }
        state.signed_out_retirement = None;
        state.unconfirmed_cleanup = carry_unconfirmed;
        state.access = Some(Access {
            token: tokens.access_token.clone(),
            expires_at,
        });
        // A refresh still in flight from the session before this one is
        // now stale.
        state.generation += 1;
        Ok(superseded)
    }

    fn expired(&self, attempt: &Attempt) -> bool {
        self.now().saturating_sub(attempt.started_at) > ATTEMPT_LIFETIME_SECS
    }

    /// Whether the status reports a sign-in as pending: an attempt that is
    /// waiting for its callback and has not expired, or a claimed attempt
    /// that is still completing.
    pub(super) fn sign_in_pending(&self, state: &super::State) -> bool {
        let awaiting = state
            .attempt
            .as_ref()
            .is_some_and(|attempt| !self.expired(attempt));
        awaiting || state.completing.is_some()
    }
}

/// Compares two strings without revealing where they differ: both are
/// hashed, and the digests are compared in full.
fn same(left: &str, right: &str) -> bool {
    let (left, right) = (
        Sha256::digest(left.as_bytes()),
        Sha256::digest(right.as_bytes()),
    );
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// An OAuth error code when it is a plain lowercase word, else a fixed
/// placeholder: the callback's text is never echoed.
fn plain_code(error: &str) -> String {
    let plain = (1..=40).contains(&error.len())
        && error
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
    if plain {
        error.to_owned()
    } else {
        "unrecognized_error".to_owned()
    }
}

/// A fresh `urn:uuid:` version-4 `ext_agent_host_id`.
pub(super) fn new_agent_host_id() -> Option<String> {
    let mut bytes = [0_u8; 16];
    aws_lc_rs::rand::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::from("urn:uuid:");
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        let _ = write!(id, "{byte:02x}");
    }
    Some(id)
}
