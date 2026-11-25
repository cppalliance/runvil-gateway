//! The `/admin/config` routes: `GET` renders the running global
//! configuration as JSON with secrets redacted; `PUT` stages the pending
//! global TOML document as a shadow file.
//!
//! The write stages the document beside its real file (`gateway.toml`
//! gains `gateway.toml.next`) without touching the real file or reloading
//! the gateway. The body is the config JSON shape `GET /admin/config`
//! returns; secrets arriving as `"***"` preserve the existing value, and
//! the merged pending configuration is validated before any shadow is
//! written, so a bad save leaves nothing behind. The shadow mechanics live
//! in `gateway-config`; these handlers own auth, path resolution, and the
//! JSON-to-TOML boundary.

use std::path::Path;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use gateway_config::save_config_shadow;
use serde::Serialize;

use crate::AppState;
use crate::auth::LoopbackCaller;
use crate::config_shadow::{PendingEnv, bytes_revision, effective_config_revision};
use crate::error::{GatewayError, WireJson, blocking, config_write_error};
use crate::registry::RouteInfo;

pub(super) const REVISION_HEADER: &str = "x-config-revision";

/// The reply of every shadow-write route: the shadow file the save
/// staged, as a display path.
#[derive(Debug, Serialize)]
pub(super) struct ShadowReply {
    shadow: String,
}

impl ShadowReply {
    /// The reply naming `shadow`.
    pub(super) fn staged(shadow: &Path) -> ShadowReply {
        ShadowReply {
            shadow: shadow.display().to_string(),
        }
    }
}

const CONFIG: RouteInfo = RouteInfo::walled("/admin/config", &[Method::GET, Method::PUT]);

/// The `/admin/config` routes, as the registry sees them.
pub(super) const ROUTES: &[RouteInfo] = &[CONFIG];

/// The `/admin/config` routes.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(CONFIG.path, get(admin_config).put(admin_put_config))
}

/// The `GET /admin/config` route: bearer-authed, renders the running global
/// config in the pending admin shape. The running profile is not part of
/// the document (`GET /admin/status` reports it), so the reply round-trips
/// through `PUT /admin/config` unchanged.
async fn admin_config(
    State(state): State<AppState>,
    _caller: LoopbackCaller,
) -> Result<Response, GatewayError> {
    let _publication = state.apply.lock().await;
    let has_path = crate::admin::config_path(&state).is_ok();
    let (body, revision) = {
        let live = state.live.read().await;
        let rev = has_path
            .then(|| live.config.source_bytes().map(bytes_revision))
            .flatten();
        (live.config.to_json(), rev)
    };
    let mut response = Json(body).into_response();
    if let Some(rev) = revision {
        if let Ok(etag) = HeaderValue::from_str(&format!("\"{rev}\"")) {
            response.headers_mut().insert(header::ETAG, etag);
        }
        if let Ok(value) = HeaderValue::from_str(&rev) {
            response.headers_mut().insert(REVISION_HEADER, value);
        }
    }
    Ok(response)
}

/// The `PUT /admin/config` route: bearer-authed, stages the global config.
///
/// The body is the full `GET /admin/config` JSON shape. Redacted `"***"`
/// secrets are restored from the current pending chain, the merged result
/// is validated like a real load, and only then is the shadow written
/// atomically. The real file stays untouched and nothing reloads. The reply
/// is `{"shadow": path}`. A body containing `active_profile` is rejected
/// as a config-write error: selection belongs to
/// `POST /admin/switch-profile`. Every save requires a non-empty `revision`
/// precondition (in the JSON body or via `If-Match` / `x-config-revision`);
/// omitting it replies `428 Precondition Required` (`config_revision_required`),
/// and a mismatch against the effective on-disk config replies `409 Conflict`
/// (`config_conflict`) without staging anything.
async fn admin_put_config(
    State(state): State<AppState>,
    _caller: LoopbackCaller,
    headers: HeaderMap,
    WireJson(mut body): WireJson<serde_json::Value>,
) -> Result<Json<ShadowReply>, GatewayError> {
    // Saves take the apply lock: apply promotes shadows without
    // re-validating, so the combination it promotes must be one the latest
    // save validated whole - saves serialize with apply, revert, and each
    // other.
    let _guard = state.apply.lock().await;
    let config_path = crate::admin::config_path(&state)?.to_path_buf();
    let expected_revision = extract_expected_revision(&headers, &mut body)?;
    let document = toml_document(body)?;
    let shadows = blocking(move || {
        let current = effective_config_revision(&config_path)?;
        if expected_revision != current {
            return Err(GatewayError::ConfigConflict);
        }
        let pending_env = PendingEnv::new(&config_path)?;
        save_config_shadow(&config_path, document, &|name: &str| {
            pending_env.resolve_var(name)
        })
        .map_err(config_write_error)
    })
    .await??;
    Ok(Json(ShadowReply::staged(&shadows.config)))
}

fn extract_expected_revision(
    headers: &HeaderMap,
    body: &mut serde_json::Value,
) -> Result<String, GatewayError> {
    let body_rev = match body.as_object_mut().and_then(|map| map.remove("revision")) {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) if !text.trim().is_empty() => Some(text),
        Some(_) => {
            return Err(GatewayError::ConfigWriteRejected(
                "revision must be a non-empty string".to_owned(),
            ));
        }
    };
    let header_rev = extract_header_revision(headers)?;
    if let (Some(from_body), Some(from_header)) = (&body_rev, &header_rev)
        && from_body != from_header
    {
        return Err(GatewayError::ConfigWriteRejected(
            "conflicting revision values in header and body".to_owned(),
        ));
    }
    body_rev
        .or(header_rev)
        .ok_or(GatewayError::ConfigRevisionRequired)
}

fn extract_header_revision(headers: &HeaderMap) -> Result<Option<String>, GatewayError> {
    let custom = parse_revision_header(headers.get(REVISION_HEADER), false)?;
    let if_match = parse_revision_header(headers.get(header::IF_MATCH), true)?;
    if let (Some(a), Some(b)) = (&custom, &if_match)
        && a != b
    {
        return Err(GatewayError::ConfigWriteRejected(
            "conflicting revision headers".to_owned(),
        ));
    }
    Ok(custom.or(if_match))
}

fn parse_revision_header(
    raw: Option<&HeaderValue>,
    strip_quotes: bool,
) -> Result<Option<String>, GatewayError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| GatewayError::ConfigWriteRejected("invalid revision header".to_owned()))?
        .trim();
    let cleaned = if strip_quotes {
        text.strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(text)
    } else {
        text
    };
    if cleaned.is_empty() {
        return Err(GatewayError::ConfigWriteRejected(
            "revision header must not be empty".to_owned(),
        ));
    }
    Ok(Some(cleaned.to_owned()))
}

/// Converts the request body into the TOML document a shadow save takes.
fn toml_document(body: serde_json::Value) -> Result<toml::Value, GatewayError> {
    let value = json_to_toml(body)?.ok_or_else(|| {
        GatewayError::ConfigWriteRejected("the body must be a JSON object".to_owned())
    })?;
    if value.is_table() {
        Ok(value)
    } else {
        Err(GatewayError::ConfigWriteRejected(
            "the body must be a JSON object".to_owned(),
        ))
    }
}

/// Converts a JSON value into a TOML one. `None` means "absent": TOML has
/// no null, so a null object member simply drops out (the serializer skips
/// absent optionals on the way out, and the deserializer defaults them on
/// the way back in). A null inside an array is an error, as is a number
/// outside TOML's ranges.
fn json_to_toml(value: serde_json::Value) -> Result<Option<toml::Value>, GatewayError> {
    Ok(Some(match value {
        serde_json::Value::Null => return Ok(None),
        serde_json::Value::Bool(flag) => toml::Value::Boolean(flag),
        serde_json::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                toml::Value::Integer(integer)
            } else if let Some(float) = number.as_f64() {
                toml::Value::Float(float)
            } else {
                return Err(GatewayError::ConfigWriteRejected(format!(
                    "number {number} does not fit a TOML value"
                )));
            }
        }
        serde_json::Value::String(text) => toml::Value::String(text),
        serde_json::Value::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            for item in items {
                let Some(element) = json_to_toml(item)? else {
                    return Err(GatewayError::ConfigWriteRejected(
                        "null inside an array has no TOML form".to_owned(),
                    ));
                };
                converted.push(element);
            }
            toml::Value::Array(converted)
        }
        serde_json::Value::Object(members) => {
            let mut table = toml::map::Map::new();
            for (key, member) in members {
                if let Some(converted) = json_to_toml(member)? {
                    table.insert(key, converted);
                }
            }
            toml::Value::Table(table)
        }
    }))
}

#[cfg(test)]
#[path = "config-tests.rs"]
mod tests;
