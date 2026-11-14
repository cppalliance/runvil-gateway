//! The persisted sign-in record and its closed session lifecycle.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A secret string that never prints: `Debug` shows `<redacted>`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wraps a token.
    #[must_use]
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    /// The token, for the one request that must carry it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where the sign-in stands, as reported externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Signed in: a refresh token is held.
    Active,
    /// Signed out by the user. The registration is kept for the next sign-in.
    SignedOut,
    /// The credential can no longer be trusted (a refused refresh, a refresh
    /// with an unknown outcome): the user must sign in again.
    NeedsAuth,
}

/// What the last remote revocation is known to have done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Revocation {
    /// OpenAI answered the revoke with HTTP 200.
    Confirmed,
    /// The revoke failed or timed out: the grant may still be live.
    Unconfirmed,
}

/// The credentials of a live sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// The refresh token held for the next rotation.
    pub refresh_token: Secret,
    /// The scopes the last token response granted.
    pub scopes: Vec<String>,
}

/// The mutually exclusive state of a ChatGPT sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Session {
    /// Signed out by the user, or not yet signed in. No token is held.
    SignedOut {
        /// The last sign-out's remote outcome, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revocation: Option<Revocation>,
    },
    /// Signed in: a refresh token is held and no refresh is in flight.
    Active(Credentials),
    /// Saved before a refresh request is sent. Found on disk at start, its
    /// outcome is unknown, so [`Session::NeedsAuth`] replaces it without
    /// retrying.
    Refreshing(Credentials),
    /// The credential can no longer be trusted; the user must sign in again.
    NeedsAuth {
        /// Kept when the stored token may still be live at the issuer (an
        /// unknown refresh outcome or a rejected non-rotating refresh), so
        /// sign-out or a new sign-in can still attempt revocation; never used
        /// to refresh.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh_token: Option<Secret>,
    },
}

impl Session {
    /// The external [`Status`] this session reports.
    #[must_use]
    pub fn status(&self) -> Status {
        match self {
            Session::SignedOut { .. } => Status::SignedOut,
            Session::Active(_) | Session::Refreshing(_) => Status::Active,
            Session::NeedsAuth { .. } => Status::NeedsAuth,
        }
    }

    /// Any refresh token still held, whether usable or awaiting revocation.
    #[must_use]
    pub fn refresh_token(&self) -> Option<&Secret> {
        match self {
            Session::SignedOut { .. } => None,
            Session::Active(creds) | Session::Refreshing(creds) => Some(&creds.refresh_token),
            Session::NeedsAuth { refresh_token } => refresh_token.as_ref(),
        }
    }

    /// The granted scopes while signed in; empty otherwise.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        match self {
            Session::Active(creds) | Session::Refreshing(creds) => &creds.scopes,
            Session::SignedOut { .. } | Session::NeedsAuth { .. } => &[],
        }
    }

    /// The last sign-out's remote outcome, while signed out.
    #[must_use]
    pub fn revocation(&self) -> Option<Revocation> {
        match self {
            Session::SignedOut { revocation } => *revocation,
            Session::Active(_) | Session::Refreshing(_) | Session::NeedsAuth { .. } => None,
        }
    }
}

/// The persisted sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The record's format.
    pub version: u32,
    /// The install's `ext_agent_host_id`, `urn:uuid:…`. Kept across
    /// sign-outs.
    pub agent_host_id: String,
    /// The issued `oaiapp_…` client id, once registered.
    pub client_id: Option<String>,
    /// The account the registration belongs to (`sub`).
    pub subject: Option<String>,
    /// The account's email, for "Signed in as" and the next `login_hint`.
    pub email: Option<String>,
    /// Where the sign-in stands and the credentials it holds, if any.
    pub session: Session,
    /// A refresh token the broker did not keep could not be confirmed
    /// revoked, so it may still be live at the issuer. Only a reset clears
    /// this: the remedy is to disconnect the app in ChatGPT settings.
    #[serde(default)]
    pub unconfirmed_cleanup: bool,
}

impl Record {
    /// The current format.
    pub const VERSION: u32 = 1;

    /// A first record for a new install, with its `agent_host_id` and
    /// nothing signed in.
    #[must_use]
    pub fn new(agent_host_id: String) -> Record {
        Record {
            version: Record::VERSION,
            agent_host_id,
            client_id: None,
            subject: None,
            email: None,
            session: Session::SignedOut { revocation: None },
            unconfirmed_cleanup: false,
        }
    }

    /// Whether the registration fields agree with [`Record::session`].
    pub(super) fn is_consistent(&self) -> bool {
        let has_client = self.client_id.as_deref().is_some_and(|id| !id.is_empty());
        let has_subject = self.subject.as_deref().is_some_and(|sub| !sub.is_empty());
        match &self.session {
            Session::SignedOut { .. } => {
                (self.subject.is_none() && self.email.is_none()) || has_client
            }
            Session::Active(_) | Session::Refreshing(_) | Session::NeedsAuth { .. } => {
                has_client && has_subject
            }
        }
    }
}
