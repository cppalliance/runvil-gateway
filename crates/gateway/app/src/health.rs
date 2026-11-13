//! The `GET /health` liveness probe.

use axum::http::Method;
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::Serialize;

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
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct HealthReply {
    /// `serving` while the gateway is answering requests.
    status: &'static str,
}

/// Liveness probe: answers 200 with `status` set to `serving` and needs no
/// credential. Like every route, it answers 403 while the gateway is bound to
/// a loopback address and the request's host is neither that address nor
/// `localhost` on the same port.
async fn health() -> Json<HealthReply> {
    Json(HealthReply { status: "serving" })
}

#[cfg(test)]
#[path = "health-tests.rs"]
mod tests;
