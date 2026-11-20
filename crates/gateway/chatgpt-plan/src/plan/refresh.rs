//! Refreshing the access token.
//!
//! One refresh runs at a time, and every caller that needs a token while it
//! runs shares its result. Admission is one critical section: it decides
//! whether the cached token still serves, and if not, saves the record as
//! [`Session::Refreshing`] before anything is sent. Each outcome is then
//! handled by what it proves:
//!
//! - the request never left: nothing changed at the issuer, so the session
//!   returns to [`Session::Active`];
//! - the issuer refused without ending the session: the same;
//! - the refresh token is dead, or no readable reply came back: the stored
//!   token may be spent, so the session becomes [`Session::NeedsAuth`] and
//!   nothing is retried;
//! - a reply arrived: it is saved only if the generation is still the one
//!   the refresh started under, and revoked otherwise.
//!
//! Reference: OpenAI's "Token reference", sections "Token response and
//! refresh tokens" and "Token lifetimes",
//! <https://developers.openai.com/siwc/token-sharing-open-source/token-reference>.

use std::sync::Arc;

use super::{Access, Inner, PlanError, REFRESH_MARGIN_SECS, State, grants_plan};
use crate::oauth::{OAuthError, TokenSet};
use crate::store::{Record, Secret, Session};

impl Inner {
    /// The cached access token when it is usable, `None` when a refresh is
    /// needed, or the reason no token may be handed out.
    pub(super) fn cached_access(&self) -> Result<Option<Secret>, PlanError> {
        self.usable(&self.lock())
    }

    pub(super) fn usable(&self, state: &State) -> Result<Option<Secret>, PlanError> {
        if state.unreadable {
            return Err(PlanError::Unreadable);
        }
        let record = state.record.as_ref().ok_or(PlanError::NotSignedIn)?;
        match &record.session {
            Session::Active(_) | Session::Refreshing(_) => {}
            Session::SignedOut { .. } => return Err(PlanError::NotSignedIn),
            Session::NeedsAuth { .. } => return Err(PlanError::NeedsAuth),
        }
        if !grants_plan(record) {
            return Err(PlanError::PlanNotGranted);
        }
        let now = self.now();
        Ok(state
            .access
            .as_ref()
            .filter(|access| access.expires_at > now.saturating_add(REFRESH_MARGIN_SECS))
            .map(|access| access.token.clone()))
    }

    /// One refresh, shared by every concurrent caller.
    pub(super) async fn refresh(self: Arc<Inner>) -> Result<Secret, PlanError> {
        let _gate = self.refresh_gate.lock().await;
        // Admission: the usability check and the marker are one critical
        // section, so nothing can change the sign-in between them.
        let (client_id, refresh_token, subject, generation) = {
            let mut state = self.lock();
            if let Some(token) = self.usable(&state)? {
                return Ok(token);
            }
            let generation = state.generation;
            let record = state.record.as_mut().ok_or(PlanError::NotSignedIn)?;
            let (Some(client_id), Some(subject)) =
                (record.client_id.clone(), record.subject.clone())
            else {
                return Err(PlanError::NeedsAuth);
            };
            let creds = match &record.session {
                Session::Active(creds) | Session::Refreshing(creds) => creds.clone(),
                Session::SignedOut { .. } => return Err(PlanError::NotSignedIn),
                Session::NeedsAuth { .. } => return Err(PlanError::NeedsAuth),
            };
            // Saved first, published second: a failed save changes nothing.
            let mut marked = record.clone();
            marked.session = Session::Refreshing(creds.clone());
            self.store.save(&marked)?;
            *record = marked;
            (client_id, creds.refresh_token, subject, generation)
        };

        let tokens = match self.oauth.refresh(&client_id, &refresh_token).await {
            Ok(tokens) => tokens,
            // The request never left: the refresh token is unspent.
            Err(error @ OAuthError::Connect) => {
                self.settle(generation, restore_active);
                return Err(error.into());
            }
            // The issuer answered and refused, without rotating.
            Err(error @ OAuthError::Refused { .. }) if !error.ends_the_session() => {
                self.settle(generation, restore_active);
                return Err(error.into());
            }
            // The refresh token is dead, or the outcome is unknown.
            Err(error) => {
                let dead = error.ends_the_session();
                tracing::warn!(
                    refused = dead,
                    "ChatGPT token refresh failed; sign in again"
                );
                self.settle(generation, |record| {
                    let kept = (!dead)
                        .then(|| record.session.refresh_token().cloned())
                        .flatten();
                    record.session = Session::NeedsAuth {
                        refresh_token: kept,
                    };
                });
                return Err(PlanError::NeedsAuth);
            }
        };

        // A refresh may omit the ID token. One that is present must verify
        // and name the same account.
        if let Some(id_token) = &tokens.id_token {
            let verified = self
                .verify(
                    id_token.expose(),
                    &client_id,
                    None,
                    tokens.access_token.expose(),
                )
                .await;
            let same_account = matches!(&verified, Ok(claims) if claims.subject == subject);
            if !same_account {
                tracing::warn!("a refreshed ChatGPT ID token was rejected; sign in again");
                let rotated = tokens.refresh_token.is_some();
                self.settle(generation, |record| {
                    let kept = (!rotated)
                        .then(|| record.session.refresh_token().cloned())
                        .flatten();
                    record.session = Session::NeedsAuth {
                        refresh_token: kept,
                    };
                });
                self.dispose(&client_id, tokens.refresh_token.as_ref())
                    .await;
                return Err(PlanError::NeedsAuth);
            }
        }

        match self.commit_refresh(generation, &tokens) {
            Ok(true) => Ok(tokens.access_token),
            Ok(false) => Err(PlanError::PlanNotGranted),
            Err(error) => {
                // Stale, or unsaved: the new token is revoked, not kept.
                self.dispose(&client_id, tokens.refresh_token.as_ref())
                    .await;
                Err(error)
            }
        }
    }

    /// Saves a refresh's result and caches its access token, when
    /// `generation` is still current, and says whether plan usage is still
    /// granted. The generation check and the save are one critical section,
    /// so a sign-out cannot slip between them.
    fn commit_refresh(&self, generation: u64, tokens: &TokenSet) -> Result<bool, PlanError> {
        let mut state = self.lock();
        if state.generation != generation {
            return Err(PlanError::Superseded);
        }
        let expires_at = self.now().saturating_add(tokens.expires_in);
        let record = state.record.as_mut().ok_or(PlanError::NotSignedIn)?;
        let (Session::Refreshing(current) | Session::Active(current)) = &record.session else {
            return Err(PlanError::NeedsAuth);
        };
        let mut creds = current.clone();
        if let Some(rotated) = &tokens.refresh_token {
            creds.refresh_token = rotated.clone();
        }
        // A reply that names no scope leaves the granted scopes unchanged.
        if let Some(scopes) = &tokens.scopes {
            creds.scopes.clone_from(scopes);
        }
        let before = record.clone();
        record.session = Session::Active(creds);
        if let Err(error) = self.store.save(record) {
            // The disk still holds the spent token and the marker, so a
            // restart will ask for a new sign-in. Do the same here.
            *record = before;
            record.session = Session::NeedsAuth {
                refresh_token: record.session.refresh_token().cloned(),
            };
            state.access = None;
            return Err(error.into());
        }
        let granted = grants_plan(record);
        state.access = Some(Access {
            token: tokens.access_token.clone(),
            expires_at,
        });
        // One line per refresh, so a log shows that the saved sign-in was
        // used. It names no token.
        tracing::info!(
            expires_in_secs = tokens.expires_in,
            plan_granted = granted,
            "refreshed the ChatGPT access token"
        );
        Ok(granted)
    }

    /// Applies `change` to the record and saves it, when `generation` is
    /// still current. A stale caller changes nothing.
    fn settle(&self, generation: u64, change: impl FnOnce(&mut Record)) {
        let mut state = self.lock();
        if state.generation != generation {
            return;
        }
        if let Some(record) = state.record.as_mut() {
            change(record);
            if !matches!(record.session, Session::Active(_)) {
                state.access = None;
            }
            if let Some(record) = state.record.as_ref()
                && self.store.save(record).is_err()
            {
                state.unreadable = true;
            }
        }
    }
}

fn restore_active(record: &mut Record) {
    if let Session::Refreshing(creds) = &record.session {
        record.session = Session::Active(creds.clone());
    }
}
