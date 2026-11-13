//! Error responses, read from `GatewayError::into_response` so the status,
//! the `Retry-After`, and the envelope's `type` and `code` are the ones the
//! route sends. Descriptions are the variant's doc comment.

use std::collections::BTreeMap;

use axum::response::IntoResponse as _;
use gateway_protocol::ProtocolError;
use serde_json::{Map, Value, json};

use super::render::object;
use super::source;
use crate::error::GatewayError;

const ERRORS: &str = "crates/gateway/app/src/error.rs";
const PROTOCOL: &str = "crates/gateway/protocol/src/error.rs";
const BATCH: &str = "crates/gateway/stt/api/src/batch.rs";
const REALTIME: &str = "crates/gateway/stt/api/src/realtime/route.rs";
const HOST: &str = "crates/shared-loopback/src/host.rs";
const AUTH: &str = "crates/gateway/app/src/auth.rs";
const ORIGIN: &str = "crates/shared-loopback/src/origin.rs";

/// One failure a route can produce, named so the operation table can list it.
#[derive(Clone, Copy, Debug)]
pub(super) enum Variant {
    Unauthorized,
    Malformed,
    UnknownModel,
    KindMismatch,
    InvalidVoice,
    ModelLoading,
    ModelProvisioning,
    QueueFull,
    QueueRejected,
    UpstreamTransport,
    UpstreamConnect,
    UpstreamProtocol,
    UpstreamServer,
    /// Any 4xx the upstream returned. The response key is the range `4XX`.
    UpstreamClient,
    ModelUnavailable,
    #[cfg(feature = "web-search")]
    ToolNotConfigured,
    Cache,
    Blocking,
    CacheMissing,
    UpstreamRateLimited,
    UpstreamUnavailable,
}

/// Every variant the operation table can name, in a stable order.
pub(super) fn all_variants() -> &'static [Variant] {
    &[
        Variant::Unauthorized,
        Variant::Malformed,
        Variant::UnknownModel,
        Variant::KindMismatch,
        Variant::InvalidVoice,
        Variant::ModelLoading,
        Variant::ModelProvisioning,
        Variant::QueueFull,
        Variant::QueueRejected,
        Variant::UpstreamTransport,
        Variant::UpstreamConnect,
        Variant::UpstreamProtocol,
        Variant::UpstreamServer,
        Variant::UpstreamClient,
        Variant::ModelUnavailable,
        #[cfg(feature = "web-search")]
        Variant::ToolNotConfigured,
        Variant::Cache,
        Variant::Blocking,
        Variant::CacheMissing,
        Variant::UpstreamRateLimited,
        Variant::UpstreamUnavailable,
    ]
}

/// The status and JSON body `variant` produces through `into_response`.
pub(super) fn envelope_body(variant: Variant) -> Result<(u16, Value), String> {
    let (error, _, _, variant_name, _) = sample(variant);
    let response = error.into_response();
    let status = response.status().as_u16();
    let bytes = body_bytes(response.into_body())?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{variant_name}: the envelope is not JSON: {error}"))?;
    Ok((status, value))
}

/// The shared envelope schema. The description is `GatewayError::envelope`'s
/// doc comment; the three builders emit this shape.
pub(super) fn envelope_schema() -> Result<Value, String> {
    let description =
        source::doc(ERRORS, "envelope").map_err(|error| error.message(ERRORS, "envelope"))?;
    Ok(json!({
        "additionalProperties": false,
        "description": description,
        "properties": {
            "error": {
                "additionalProperties": false,
                "properties": {
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "type": { "type": "string" }
                },
                "required": ["message", "type", "code"],
                "type": "object"
            }
        },
        "required": ["error"],
        "type": "object"
    }))
}

/// The `responses` entries for `variants`, grouped by status. `4XX` is the
/// range key for an upstream client status.
pub(super) fn responses(variants: &[Variant]) -> Result<Map<String, Value>, String> {
    let mut grouped: BTreeMap<String, Vec<Piece>> = BTreeMap::new();
    for variant in variants {
        let piece = piece(*variant)?;
        grouped.entry(piece.key.clone()).or_default().push(piece);
    }
    let mut responses = object();
    for (key, pieces) in grouped {
        responses.insert(key, response(&pieces)?);
    }
    Ok(responses)
}

/// The host wall's 403, from `require_loopback_host`.
pub(super) fn host_wall() -> Result<Value, String> {
    let doc = source::doc(HOST, "require_loopback_host")
        .map_err(|error| error.message(HOST, "require_loopback_host"))?;
    Ok(json!({
        "description": doc,
        "x-runvil-source": format!("{HOST}#require_loopback_host")
    }))
}

/// The batch route's statuses, parsed from `error_response` so a new arm
/// shows up here instead of being skipped. Each is described by the
/// paragraph of `error_response`'s doc that opens with its code.
pub(super) fn batch_responses() -> Result<Map<String, Value>, String> {
    let triples = batch_triples()?;
    let mut responses = object();
    for (status, kind, code) in triples {
        let key = status_name_to_code(&status)?;
        let description = super::fields::labeled_paragraph(BATCH, "error_response", &code)?;
        responses.insert(
            key,
            json!({
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorEnvelope" } } },
                "description": description,
                "x-runvil-error-codes": [code],
                "x-runvil-error-types": [kind],
                "x-runvil-source": format!("{BATCH}#error_response")
            }),
        );
    }
    Ok(responses)
}

/// The numeric statuses the realtime `upgrade` function returns alone.
pub(super) fn realtime_codes() -> Result<Vec<String>, String> {
    status_names(REALTIME, "upgrade")?
        .into_iter()
        .map(|name| status_name_to_code(&name))
        .collect()
}

/// The realtime upgrade's pre-upgrade refusals: each `StatusCode` the
/// function returns alone, which is an empty body. Each is described as
/// `No body when` followed by the paragraph of `upgrade`'s doc that opens
/// with the numeric status, so that paragraph scopes the empty body.
pub(super) fn realtime_responses() -> Result<Map<String, Value>, String> {
    let mut responses = object();
    for key in realtime_codes()? {
        let paragraph = super::fields::labeled_paragraph(REALTIME, "upgrade", &key)?;
        responses.insert(
            key,
            json!({
                "description": format!("No body when {paragraph}"),
                "x-runvil-source": format!("{REALTIME}#upgrade")
            }),
        );
    }
    Ok(responses)
}

/// The realtime handshake's `Origin` refusal, from `authorize_stt_route`: a
/// bare 403 after authentication, described by that function's doc and by
/// the shared Origin predicate it applies.
pub(super) fn realtime_origin() -> Result<Value, String> {
    let route = source::doc(AUTH, "authorize_stt_route")
        .map_err(|error| error.message(AUTH, "authorize_stt_route"))?;
    let rule = "gateway_loopback_origin_allowed";
    let origin = source::doc(ORIGIN, rule).map_err(|error| error.message(ORIGIN, rule))?;
    Ok(json!({
        "description": format!("{route}\n\n{origin}"),
        "x-runvil-source": format!("{AUTH}#authorize_stt_route")
    }))
}

struct Piece {
    key: String,
    code: String,
    doc: String,
    retry_after: Option<u16>,
}

fn piece(variant: Variant) -> Result<Piece, String> {
    let (error, file, enum_name, variant_name, range) = sample(variant);
    let response = error.into_response();
    let status = response.status();
    let retry_after = match response.headers().get(axum::http::header::RETRY_AFTER) {
        None => None,
        Some(value) => {
            let text = value
                .to_str()
                .map_err(|error| format!("{variant_name}: Retry-After is not text: {error}"))?;
            Some(text.parse::<u16>().map_err(|error| {
                format!("{variant_name}: Retry-After is not an integer number of seconds: {error}")
            })?)
        }
    };
    let bytes = body_bytes(response.into_body())?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{variant_name}: the envelope is not JSON: {error}"))?;
    let code = value["error"]["code"]
        .as_str()
        .ok_or_else(|| format!("{variant_name}: the envelope has no code"))?
        .to_owned();
    let doc = source::variant_doc(file, enum_name, variant_name)
        .map_err(|error| error.message(file, variant_name))?;
    if range && !status.is_client_error() {
        return Err(format!(
            "{variant_name}: an upstream client status came back as {status}"
        ));
    }
    let key = if range {
        "4XX".to_owned()
    } else {
        status.as_u16().to_string()
    };
    Ok(Piece {
        key,
        code,
        doc,
        retry_after,
    })
}

fn sample(variant: Variant) -> (GatewayError, &'static str, &'static str, &'static str, bool) {
    let gateway = |error, name| (error, ERRORS, "GatewayError", name, false);
    match variant {
        Variant::Unauthorized => gateway(GatewayError::Unauthorized, "Unauthorized"),
        Variant::Malformed => gateway(
            GatewayError::MalformedRequest("malformed".to_owned()),
            "MalformedRequest",
        ),
        Variant::UnknownModel => gateway(
            GatewayError::UnknownModel("missing".to_owned()),
            "UnknownModel",
        ),
        Variant::KindMismatch => gateway(
            GatewayError::KindMismatch {
                model: "m".to_owned(),
                expected: gateway_config::ModelKind::Chat,
                actual: gateway_config::ModelKind::Embedding,
            },
            "KindMismatch",
        ),
        Variant::InvalidVoice => gateway(
            GatewayError::InvalidVoice {
                voice: "nope".to_owned(),
                valid: vec!["one".to_owned()],
            },
            "InvalidVoice",
        ),
        Variant::ModelLoading => {
            gateway(GatewayError::ModelLoading("m".to_owned()), "ModelLoading")
        }
        Variant::ModelProvisioning => gateway(
            GatewayError::ModelProvisioning("load".to_owned()),
            "ModelProvisioning",
        ),
        Variant::QueueFull => gateway(GatewayError::QueueFull, "QueueFull"),
        Variant::QueueRejected => gateway(GatewayError::QueueRejected, "QueueRejected"),
        Variant::UpstreamTransport => protocol(
            ProtocolError::transport(std::io::Error::other("reset")),
            "UpstreamTransport",
            false,
        ),
        Variant::UpstreamConnect => protocol(
            ProtocolError::connect(std::io::Error::other("refused")),
            "UpstreamConnect",
            false,
        ),
        Variant::UpstreamProtocol => protocol(
            ProtocolError::upstream_protocol(std::io::Error::other("bad")),
            "UpstreamProtocol",
            false,
        ),
        Variant::UpstreamServer => protocol(
            ProtocolError::upstream_status(500, "no".to_owned()),
            "UpstreamStatus",
            false,
        ),
        Variant::UpstreamClient => protocol(
            ProtocolError::upstream_status(418, "no".to_owned()),
            "UpstreamStatus",
            true,
        ),
        Variant::ModelUnavailable => protocol(
            ProtocolError::ModelUnavailable("m".to_owned()),
            "ModelUnavailable",
            false,
        ),
        #[cfg(feature = "web-search")]
        Variant::ToolNotConfigured => gateway(
            GatewayError::ToolNotConfigured("web_search"),
            "ToolNotConfigured",
        ),
        Variant::Cache => gateway(GatewayError::cache(std::io::Error::other("disk")), "Cache"),
        Variant::Blocking => gateway(
            GatewayError::BlockingTask(Box::new(std::io::Error::other("join"))),
            "BlockingTask",
        ),
        Variant::CacheMissing => gateway(
            GatewayError::CacheEntryNotFound("ab".to_owned()),
            "CacheEntryNotFound",
        ),
        Variant::UpstreamRateLimited => {
            gateway(GatewayError::UpstreamRateLimited, "UpstreamRateLimited")
        }
        Variant::UpstreamUnavailable => {
            gateway(GatewayError::UpstreamUnavailable, "UpstreamUnavailable")
        }
    }
}

fn protocol(
    error: ProtocolError,
    name: &'static str,
    range: bool,
) -> (GatewayError, &'static str, &'static str, &'static str, bool) {
    (
        GatewayError::Protocol(error),
        PROTOCOL,
        "ProtocolError",
        name,
        range,
    )
}

fn response(pieces: &[Piece]) -> Result<Value, String> {
    let mut codes: Vec<&str> = pieces.iter().map(|piece| piece.code.as_str()).collect();
    codes.sort_unstable();
    codes.dedup();
    let description = pieces
        .iter()
        .map(|piece| piece.doc.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut response = json!({
        "content": {
            "application/json": {
                "schema": { "$ref": "#/components/schemas/ErrorEnvelope" }
            }
        },
        "description": description,
        "x-runvil-error-codes": codes
    });
    if let Some(seconds) = pieces.iter().find_map(|piece| piece.retry_after) {
        let description = source::doc(ERRORS, "MODEL_LOADING_RETRY_AFTER_SECONDS")
            .map_err(|error| error.message(ERRORS, "MODEL_LOADING_RETRY_AFTER_SECONDS"))?;
        response.as_object_mut().expect("a response object").insert(
            "headers".to_owned(),
            json!({
                "Retry-After": {
                    "description": description,
                    "schema": { "const": seconds, "type": "integer" }
                }
            }),
        );
    }
    Ok(response)
}

fn body_bytes(body: axum::body::Body) -> Result<Vec<u8>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("the error reader could not start a runtime: {error}"))?;
    let bytes = runtime
        .block_on(axum::body::to_bytes(body, 1024 * 1024))
        .map_err(|error| format!("the envelope body did not read: {error}"))?;
    Ok(bytes.to_vec())
}

/// `(STATUS_NAME, type, code)` triples in `error_response`, in source order.
fn batch_triples() -> Result<Vec<(String, String, String)>, String> {
    let text = std::fs::read_to_string(source::root().join(BATCH))
        .map_err(|error| format!("{BATCH}: {error}"))?;
    let mut triples = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(status) = line.trim().strip_prefix("StatusCode::") else {
            continue;
        };
        let status = status.trim_end_matches(',').to_owned();
        let Some(kind) = lines.next().and_then(string_literal) else {
            continue;
        };
        let Some(code) = lines.next().and_then(string_literal) else {
            continue;
        };
        triples.push((status, kind, code));
    }
    if triples.is_empty() {
        return Err(format!(
            "{BATCH}: `error_response` yielded no status triples"
        ));
    }
    Ok(triples)
}

fn string_literal(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_end_matches(',');
    let trimmed = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    Some(trimmed.to_owned())
}

fn status_names(file: &str, function: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(source::root().join(file))
        .map_err(|error| format!("{file}: {error}"))?;
    let marker = format!("fn {function}");
    let Some(start) = text.find(&marker) else {
        return Err(format!("{file}: no `{function}`"));
    };
    let rest = &text[start + marker.len()..];
    let end = rest
        .find("\nasync fn ")
        .or_else(|| rest.find("\nfn "))
        .unwrap_or(rest.len());
    let body = &rest[..end];
    let mut names = Vec::new();
    for token in body.split("StatusCode::").skip(1) {
        let name: String = token
            .chars()
            .take_while(|char| char.is_ascii_uppercase() || *char == '_')
            .collect();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Err(format!("{file}: `{function}` returns no StatusCode"));
    }
    Ok(names)
}

fn status_name_to_code(name: &str) -> Result<String, String> {
    let code = match name {
        "BAD_REQUEST" => 400,
        "UNAUTHORIZED" => 401,
        "FORBIDDEN" => 403,
        "NOT_FOUND" => 404,
        "PAYLOAD_TOO_LARGE" => 413,
        "TOO_MANY_REQUESTS" => 429,
        "INTERNAL_SERVER_ERROR" => 500,
        "BAD_GATEWAY" => 502,
        "SERVICE_UNAVAILABLE" => 503,
        other => return Err(format!("no status mapping for StatusCode::{other}")),
    };
    Ok(code.to_string())
}
