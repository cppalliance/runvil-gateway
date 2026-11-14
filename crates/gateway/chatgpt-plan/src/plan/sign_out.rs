//! Signing out, forgetting the account, and resetting an unreadable record.
//!
//! Sign-out is local first. The generation is bumped and the record saved as
//! signed out before the network is touched, so a stalled revocation cannot
//! keep the session usable, and any refresh or sign-in still in flight has
//! its result revoked instead of saved. The remote revocation is then
//! attempted once, bounded, and reported honestly: confirmed only when the
//! issuer answered 200.

use std::sync::Arc;

use super::{ChatGptPlan, Inner, PlanError};
use crate::store::{Record, Revocation, Session};

/// What a sign-out did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedOut {
    /// Whether the issuer confirmed the revocation. `false` covers a
    /// failed, timed-out or unneeded revocation: the grant may still be
    /// live at the issuer, and the user can disconnect the app there.
    pub revoked: bool,
}

impl ChatGptPlan {
    /// Signs out here, then revokes the refresh token at the issuer. The
    /// registration and the install's `ext_agent_host_id` are kept for the
    /// next sign-in.
    ///
    /// # Errors
    /// [`PlanError::Store`] when the signed-out record could not be saved.
    /// The sign-in is unusable in this process either way.
    pub async fn sign_out(&self) -> Result<SignedOut, PlanError> {
        let inner = self.inner.clone();
        // The broker owns the sign-out, so the revocation is attempted even
        // if the caller goes away.
        tokio::spawn(async move { inner.sign_out(false).await })
            .await
            .map_err(|_| PlanError::Interrupted("sign-out task stopped"))?
    }

    /// Signs out, then deletes the registration and the account, keeping
    /// only the install's `ext_agent_host_id`.
    ///
    /// # Errors
    /// The errors of [`ChatGptPlan::sign_out`] and of saving the record.
    pub async fn forget(&self) -> Result<SignedOut, PlanError> {
        let inner = self.inner.clone();
        tokio::spawn(async move { inner.sign_out(true).await })
            .await
            .map_err(|_| PlanError::Interrupted("forget task stopped"))?
    }

    /// Deletes the saved record, readable or not. The next sign-in starts a
    /// new install, with a new `ext_agent_host_id`. This is the way out of an
    /// unreadable record.
    ///
    /// # Errors
    /// [`PlanError::Store`] when the file cannot be removed.
    pub fn reset(&self) -> Result<(), PlanError> {
        let mut state = self.inner.lock();
        self.inner.store.remove()?;
        state.generation += 1;
        state.attempts_started += 1;
        state.record = None;
        state.access = None;
        state.attempt = None;
        state.completing = None;
        state.unreadable = false;
        state.unconfirmed_cleanup = false;
        state.signed_out_retirement = None;
        Ok(())
    }
}

impl Inner {
    /// Signs out. With `forget`, the same critical section also drops the
    /// registration and the account, so no sign-in can land between the two.
    async fn sign_out(self: Arc<Inner>, forget: bool) -> Result<SignedOut, PlanError> {
        // Local first. The generation is bumped in the same critical
        // section that signs out, so nothing in flight can read the new
        // generation while the record still looks signed in.
        let (retirement_id, held, saved) = {
            let mut state = self.lock();
            state.generation += 1;
            state.attempts_started += 1;
            state.access = None;
            state.attempt = None;
            state.completing = None;
            let Some(record) = state.record.as_mut() else {
                return Ok(SignedOut { revoked: false });
            };
            let prior_unconfirmed = matches!(
                record.session,
                Session::SignedOut {
                    revocation: Some(Revocation::Unconfirmed),
                }
            );
            let held = record
                .session
                .refresh_token()
                .cloned()
                .zip(record.client_id.clone());
            if forget {
                // An unconfirmed cleanup outlives the account it was for.
                let unconfirmed_cleanup = record.unconfirmed_cleanup;
                *record = Record::new(record.agent_host_id.clone());
                record.unconfirmed_cleanup = unconfirmed_cleanup;
            }
            record.session = Session::SignedOut {
                revocation: (held.is_some() || prior_unconfirmed)
                    .then_some(Revocation::Unconfirmed),
            };
            let saved = self.store.save(record);
            let retirement_id = if held.is_some() {
                state.next_retirement_id += 1;
                let id = state.next_retirement_id;
                state.pending_retirements.insert(id);
                state.signed_out_retirement = Some(id);
                Some(id)
            } else {
                if !prior_unconfirmed {
                    state.signed_out_retirement = None;
                }
                None
            };
            (retirement_id, held, saved)
        };
        let (Some(retirement_id), Some((refresh_token, client_id))) = (retirement_id, held) else {
            saved?;
            return Ok(SignedOut { revoked: false });
        };

        let revoked = self.oauth.revoke(&client_id, &refresh_token).await;
        tracing::info!(revoked, "signed out of ChatGPT");
        let superseded_unconfirmed = {
            let mut state = self.lock();
            state.pending_retirements.remove(&retirement_id);
            let still_owns_signed_out = state.signed_out_retirement == Some(retirement_id);
            if still_owns_signed_out {
                state.signed_out_retirement = None;
            }
            if revoked {
                if still_owns_signed_out && let Some(record) = state.record.as_mut() {
                    record.session = Session::SignedOut {
                        revocation: Some(Revocation::Confirmed),
                    };
                    let _ = self.store.save(record);
                }
                false
            } else {
                !still_owns_signed_out
            }
        };
        if superseded_unconfirmed {
            // A newer sign-in or reset superseded the signed-out record while
            // this revocation was in flight; carry the unconfirmed retirement
            // into cleanup_unconfirmed so status still shows it.
            self.note_unconfirmed_cleanup();
        }
        saved?;
        Ok(SignedOut { revoked })
    }
}
