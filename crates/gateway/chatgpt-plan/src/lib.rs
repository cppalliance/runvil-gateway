//! Manages a user's Sign in with ChatGPT (SIWC) credentials for the Gateway process.
//!
//! This crate owns the credential lifecycle—loopback callback capture,
//! ID-token verification, owner-only storage, OAuth registration/exchange,
//! refresh, and revocation—so credentials remain inside the Gateway process
//! rather than leaking to clients, routes, or logs. Authorized internal
//! adapters obtain access bearers either through
//! [`gateway_protocol::upstream::BearerSource`] (for upstream Responses
//! requests) or [`plan::ChatGptPlan::access_token`] (for the Gateway app's
//! ChatGPT model catalog fetcher). Public status snapshots and wire errors
//! never expose tokens.
//!
//! The protocol is OpenAI's documented self-serve flow for open-source local
//! tools: <https://developers.openai.com/siwc/token-sharing-open-source>. Each module names the page and section it follows.

pub mod callback;
pub mod id_token;
pub mod oauth;
pub mod plan;
pub mod store;

#[cfg(any(test, feature = "test-fixtures"))]
#[expect(
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    reason = "a test fixture fails at once when its own plumbing breaks, and tests read it"
)]
pub mod fake_issuer;
#[cfg(any(test, feature = "test-fixtures"))]
mod testing;
