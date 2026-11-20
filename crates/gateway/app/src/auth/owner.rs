//! [`OwnerCaller`]: a walled-tier caller that also presents this boot's
//! owner proof.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::LoopbackCaller;
use crate::AppState;
use crate::error::GatewayError;

/// A [`LoopbackCaller`] that presented the owner proof: the extractor every
/// handler takes that manages what holds a ChatGPT plan's credential.
///
/// The proof comes from the private file only Runvil's own programs read.
/// The shared key alone does not pass, so a tool that was handed that key
/// to call models cannot sign in, sign out, or read the sign-in's status.
/// The order is the wall's: a non-loopback peer is refused first, then
/// ordinary authentication runs, then the proof is checked.
pub(crate) struct OwnerCaller;

impl FromRequestParts<AppState> for OwnerCaller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<OwnerCaller, Response> {
        let _caller = LoopbackCaller::from_request_parts(parts, state).await?;
        if !state.chatgpt.owner_proof_presented(&parts.headers) {
            return Err(GatewayError::OwnerProofRequired.into_response());
        }
        Ok(OwnerCaller)
    }
}
