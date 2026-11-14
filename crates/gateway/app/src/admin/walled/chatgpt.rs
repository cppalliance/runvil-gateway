//! The `/admin/chatgpt/*` routes: signing a user in to ChatGPT, reporting
//! where that sign-in stands, and ending it.
//!
//! These routes manage the credential that spends a user's ChatGPT plan, so
//! each takes [`OwnerCaller`]: a loopback peer, ordinary authentication,
//! and this boot's owner proof. No route returns a token. The status never
//! holds one, and the sign-in route returns only the URL the browser opens.
//!
//! Sign-in starts a one-shot listener on `127.0.0.1` for the browser's
//! callback and returns at once. The listener hands the callback to the
//! broker and closes when the attempt completes, expires, or is replaced.
//! At most one listener is open: a new sign-in closes the one before it.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use gateway_chatgpt_plan::callback::CallbackListener;
use gateway_chatgpt_plan::plan::{Clock, PlanStatus, SignedOut};
use gateway_chatgpt_plan::store::{Revocation, Status};
use serde::Serialize;

use crate::AppState;
use crate::auth::OwnerCaller;
use crate::error::GatewayError;
use crate::registry::RouteInfo;
use crate::routing::CredentialSource;

mod models;

const SIGN_IN: RouteInfo = RouteInfo::walled("/admin/chatgpt/sign-in", &[Method::POST]);
const STATUS: RouteInfo = RouteInfo::walled("/admin/chatgpt/status", &[Method::GET]);
const SIGN_OUT: RouteInfo = RouteInfo::walled("/admin/chatgpt/sign-out", &[Method::POST]);
const FORGET: RouteInfo = RouteInfo::walled("/admin/chatgpt/forget", &[Method::POST]);
const RESET: RouteInfo = RouteInfo::walled("/admin/chatgpt/reset", &[Method::POST]);
const MODELS_ENABLE: RouteInfo = RouteInfo::walled("/admin/chatgpt/models/enable", &[Method::POST]);
const MODELS_FORGET: RouteInfo = RouteInfo::walled("/admin/chatgpt/models/forget", &[Method::POST]);

/// The ChatGPT sign-in routes, as the registry sees them.
pub(super) const ROUTES: &[RouteInfo] = &[
    SIGN_IN,
    STATUS,
    SIGN_OUT,
    FORGET,
    RESET,
    MODELS_ENABLE,
    MODELS_FORGET,
];

/// The ChatGPT sign-in routes.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(SIGN_IN.path, post(sign_in))
        .route(STATUS.path, get(status))
        .route(SIGN_OUT.path, post(sign_out))
        .route(FORGET.path, post(forget))
        .route(RESET.path, post(reset))
        .route(MODELS_ENABLE.path, post(models::enable_models))
        .route(MODELS_FORGET.path, post(models::forget_models))
}

/// The `POST /admin/chatgpt/sign-in` reply.
#[derive(Debug, Serialize)]
struct SignInReply {
    /// The URL to open in the user's browser.
    authorize_url: String,
}

/// The `GET /admin/chatgpt/status` reply. It never holds a token.
#[derive(Debug, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact a status page shows"
)]
struct StatusReply {
    /// `active`, `signed_out`, `needs_auth`, or null when nobody has ever
    /// signed in.
    status: Option<&'static str>,
    /// The saved record cannot be read and must be reset.
    unreadable: bool,
    /// The signed-in account's email.
    email: Option<String>,
    /// The account granted plan usage to this app.
    plan_granted: bool,
    /// The last sign-out's remote outcome: `confirmed` or `unconfirmed`.
    revocation: Option<&'static str>,
    /// A token the gateway did not keep may still be live at the issuer.
    cleanup_unconfirmed: bool,
    /// A sign-in is waiting for its callback.
    sign_in_pending: bool,
    /// The gateway can hold a sign-in at all.
    available: bool,
    /// Stable reason code (`starting`, `not_loopback`, `no_profile_dir`,
    /// `storage_unavailable`) when `available` is false.
    unavailable_code: Option<&'static str>,
    /// Why not, in one sentence, when `available` is false.
    unavailable_reason: Option<String>,
    /// Caller-facing model names bound to a `chatgpt-plan` endpoint in the
    /// live routing table, in catalog order.
    plan_models: Vec<String>,
}

impl StatusReply {
    fn of(plan: &PlanStatus, plan_models: Vec<String>) -> StatusReply {
        StatusReply {
            status: plan.status.map(|status| match status {
                Status::Active => "active",
                Status::SignedOut => "signed_out",
                Status::NeedsAuth => "needs_auth",
            }),
            unreadable: plan.unreadable,
            email: plan.email.clone(),
            plan_granted: plan.plan_granted,
            revocation: plan.revocation.map(|revocation| match revocation {
                Revocation::Confirmed => "confirmed",
                Revocation::Unconfirmed => "unconfirmed",
            }),
            cleanup_unconfirmed: plan.cleanup_unconfirmed,
            sign_in_pending: plan.sign_in_pending,
            available: true,
            unavailable_code: None,
            unavailable_reason: None,
            plan_models,
        }
    }

    fn unavailable(code: &'static str, reason: String) -> StatusReply {
        StatusReply {
            status: None,
            unreadable: false,
            email: None,
            plan_granted: false,
            revocation: None,
            cleanup_unconfirmed: false,
            sign_in_pending: false,
            available: false,
            unavailable_code: Some(code),
            unavailable_reason: Some(reason),
            plan_models: Vec::new(),
        }
    }
}

/// The `{"revoked": bool}` reply of sign-out and forget.
#[derive(Debug, Serialize)]
struct RevokedReply {
    /// The issuer confirmed the revocation. False covers a failed, timed
    /// out or unneeded one: the grant may still be live at the issuer.
    revoked: bool,
}

impl From<SignedOut> for RevokedReply {
    fn from(outcome: SignedOut) -> RevokedReply {
        RevokedReply {
            revoked: outcome.revoked,
        }
    }
}

/// `POST /admin/chatgpt/sign-in`: starts a sign-in and returns the URL to
/// open. A second call replaces the first.
async fn sign_in(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<Json<SignInReply>, GatewayError> {
    let plan = state.chatgpt.broker()?.clone();
    let clock: Clock = Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    });
    // One sign-in at a time reaches the broker and owns the listener slot.
    let mut current = state.chatgpt.listener().lock().await;
    let listener = CallbackListener::bind(clock)
        .await
        .map_err(|error| GatewayError::ChatGptUnavailable(error.to_string()))?;
    let authorize_url = plan.begin_sign_in(listener.redirect_uri()).await?;
    let serving = tokio::spawn(async move {
        match listener.serve(&plan).await {
            Ok(done) => {
                tracing::info!(outcome = ?done, "the ChatGPT sign-in callback listener closed");
            }
            Err(error) => {
                tracing::warn!("the ChatGPT sign-in callback listener failed: {error}");
            }
        }
    });
    // The sign-in this one replaced can no longer complete: close its
    // listener now instead of leaving its port open until it expires. The
    // broker runs an exchange in its own task, so this loses no token.
    if let Some(replaced) = current.replace(serving.abort_handle()) {
        replaced.abort();
    }
    Ok(Json(SignInReply { authorize_url }))
}

/// `GET /admin/chatgpt/status`: where the sign-in stands.
async fn status(State(state): State<AppState>, _owner: OwnerCaller) -> Json<StatusReply> {
    if let Some((code, reason)) = state.chatgpt.unavailable_info() {
        return Json(StatusReply::unavailable(code, reason));
    }
    let Ok(plan) = state.chatgpt.broker() else {
        return Json(StatusReply::unavailable(
            "storage_unavailable",
            "ChatGPT sign-in is unavailable".to_owned(),
        ));
    };
    let plan_models = state
        .live
        .read()
        .await
        .routing
        .models()
        .iter()
        .filter(|model| model.endpoint.source == CredentialSource::ChatgptPlan)
        .map(|model| model.name.clone())
        .collect();
    Json(StatusReply::of(&plan.status(), plan_models))
}

/// `POST /admin/chatgpt/sign-out`: signs out here, then revokes at the
/// issuer.
async fn sign_out(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<Json<RevokedReply>, GatewayError> {
    Ok(Json(state.chatgpt.broker()?.sign_out().await?.into()))
}

/// `POST /admin/chatgpt/forget`: signs out and drops the registration.
async fn forget(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<Json<RevokedReply>, GatewayError> {
    Ok(Json(state.chatgpt.broker()?.forget().await?.into()))
}

/// `POST /admin/chatgpt/reset`: deletes the saved record, readable or not.
async fn reset(
    State(state): State<AppState>,
    _owner: OwnerCaller,
) -> Result<StatusCode, GatewayError> {
    state.chatgpt.broker()?.reset()?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
