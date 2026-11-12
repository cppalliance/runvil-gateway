//! The `GET /health` liveness probe.

use axum::http::Method;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use utoipa::ToSchema;

use crate::AppState;
use crate::registry::RouteInfo;

const HEALTH: RouteInfo = RouteInfo::open("/health", &[Method::GET]);

/// The health route, as the registry sees it.
pub(crate) const ROUTES: &[RouteInfo] = &[HEALTH];

/// The health route.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(HEALTH.path, get(health))
}

/// The `GET /health` body: `status` is `serving` while the gateway answers.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct HealthReply {
    /// `serving` while the gateway is answering requests.
    status: &'static str,
}

/// Liveness probe; unauthenticated and always 200 while serving.
#[utoipa::path(
    get,
    path = HEALTH.path,
    operation_id = "get_health",
    tag = "health",
    security(()),
    responses(
        (status = 200, description = "The gateway is serving.", body = HealthReply),
        (status = 403, description = "No body. The host wall refused a request whose authority is not the bound loopback socket."),
    ),
    extensions(
        ("x-runvil-extension" = json!(true)),
        ("x-runvil-opt-in" = json!(false)),
    ),
)]
async fn health() -> Json<HealthReply> {
    Json(HealthReply { status: "serving" })
}

#[cfg(test)]
#[path = "health-tests.rs"]
mod tests;
