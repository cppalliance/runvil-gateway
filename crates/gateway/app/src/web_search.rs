//! The `POST /v1/tools/web_search` tool route: bearer-authed, delegates
//! to the web-search service crate. Compiled only with the `web-search`
//! feature, since the route has nothing to delegate to without it.

use axum::extract::State;
use axum::http::Method;
use axum::routing::post;
use axum::{Json, Router};
use gateway_web_search::{WebSearchRequest, WebSearchResponse};

use crate::AppState;
use crate::auth::AuthedCaller;
use crate::error::{GatewayError, WireJson};
use crate::registry::RouteInfo;
use crate::wire::ErrorEnvelope;

const WEB_SEARCH: RouteInfo = RouteInfo::open("/v1/tools/web_search", &[Method::POST]);

/// The web-search tool route, as the registry sees it.
pub(crate) const ROUTES: &[RouteInfo] = &[WEB_SEARCH];

/// The web-search tool route.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(WEB_SEARCH.path, post(web_search))
}

/// The `POST /v1/tools/web_search` route.
///
/// # Errors
/// Returns [`GatewayError::Unauthorized`] when the bearer token is absent or
/// wrong, [`GatewayError::ToolNotConfigured`] when no `[tools.web_search]`
/// section is present, [`GatewayError::MalformedRequest`] when the request
/// fails validation, and the upstream variants on a provider failure.
#[utoipa::path(
    post,
    path = WEB_SEARCH.path,
    operation_id = "post_v1_tools_web_search",
    tag = "web_search",
    request_body = WebSearchRequest,
    responses(
        (status = 200, description = "The trimmed search results.", body = WebSearchResponse),
        (status = 400, description = "The body is not JSON this route accepts or failed the service's checks.", body = ErrorEnvelope),
        (status = 401, description = "The bearer key was missing or did not match `server.key`.", body = ErrorEnvelope),
        (status = 403, description = "No body. The host wall refused a request whose authority is not the bound loopback socket."),
        (status = 404, description = "No `[tools.web_search]` section is configured.", body = ErrorEnvelope),
        (status = "4XX", description = "The search provider answered with this client error status, passed through.", body = ErrorEnvelope),
        (status = 502, description = "The search provider could not be reached, failed after the request left, answered with a server error, or sent a body that could not be decoded.", body = ErrorEnvelope),
    ),
    extensions(
        ("x-runvil-extension" = json!(true)),
        ("x-runvil-opt-in" = json!(false)),
        ("x-runvil-feature" = json!("web-search")),
        ("x-runvil-loopback-trust" = json!(true)),
    ),
)]
async fn web_search(
    State(state): State<AppState>,
    _caller: AuthedCaller,
    WireJson(request): WireJson<WebSearchRequest>,
) -> Result<Json<WebSearchResponse>, GatewayError> {
    let service = state
        .web_search()
        .await
        .ok_or(GatewayError::ToolNotConfigured("web_search"))?;
    Ok(Json(service.search(&request).await?))
}
