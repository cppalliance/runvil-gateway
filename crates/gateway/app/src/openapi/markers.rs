//! The edits made to the OpenAPI document after utoipa builds it, for the
//! facts utoipa's attributes cannot express.
//!
//! utoipa writes the document from the `#[utoipa::path]` attributes and
//! the `ToSchema` derives. A few things this document must say have no
//! attribute to carry them:
//!
//! - An `x-runvil-*` marker on a response, a media type, a whole
//!   component, a field whose schema is a `$ref`, or a schema that accepts
//!   any value. On a list field utoipa puts the marker on the items.
//! - A `const`, such as the loading 503's `Retry-After`, which is always 5.
//! - A union whose branches overlap. utoipa writes every untagged enum as
//!   `oneOf`, which rejects a value that matches more than one branch.
//!
//! So [`apply`] edits the serialized JSON. Each table below lists JSON
//! pointers and what to add at each one. It runs only in tests, where the
//! document is generated and compared with the committed `openapi.json`.
//! A row whose feature this build leaves out is skipped. Any other row
//! whose pointer does not resolve fails generation, so a renamed field or
//! a removed status cannot silently lose its marker.
//!
//! Example: a response's error codes. utoipa writes the 404 of
//! `POST /v1/embeddings` with no place for the codes a client can match
//! on. [`ERROR_CODES`] adds them:
//!
//! ```json
//! "404": {
//!   "content": {
//!     "application/json": { "schema": { "$ref": "#/components/schemas/ErrorEnvelope" } }
//!   },
//!   "description": "The request named a model with no `[[model]]` entry.",
//!   "x-runvil-error-codes": ["model_not_found"]
//! }
//! ```
//!
//! The last line is the one added here. In the same table, the 503's
//! `Retry-After` schema `{ "type": "integer", "format": "int32",
//! "minimum": 0 }` becomes `{ "type": "integer", "const": 5 }`.
//!
//! Example: an overlapping union. `/v1/models` lists `CatalogModelInfo`
//! entries, each a routed model or a speech model. utoipa writes, with the
//! descriptions left out:
//!
//! ```json
//! "CatalogModelInfo": {
//!   "oneOf": [
//!     { "$ref": "#/components/schemas/ModelInfo" },
//!     { "$ref": "#/components/schemas/SpeechCatalogModelInfo" }
//!   ]
//! }
//! ```
//!
//! `oneOf` means exactly one branch matches. But the speech branch needs
//! only `id`, `object`, and `kind` and allows other fields, so every
//! routed model matches it too, and a client validating with `oneOf`
//! would reject every routed model the server lists. [`OVERLAPPING_UNIONS`]
//! renames the key to `anyOf`, at least one branch:
//!
//! ```json
//! "CatalogModelInfo": {
//!   "anyOf": [
//!     { "$ref": "#/components/schemas/ModelInfo" },
//!     { "$ref": "#/components/schemas/SpeechCatalogModelInfo" }
//!   ]
//! }
//! ```

use serde_json::{Value, json};

use super::enabled;

/// Schemas that accept any value: the payloads passed through to and from
/// the backend unread, and the flattened maps that keep every unnamed
/// field. Each gets `x-runvil-pass-through: true`.
const PASS_THROUGH: &[(&str, Option<&str>)] = &[
    (
        "/components/schemas/ChatRequest/properties/messages/items",
        None,
    ),
    ("/components/schemas/ChatRequest/additionalProperties", None),
    (
        "/components/schemas/ChatResponse/properties/choices/items",
        None,
    ),
    (
        "/components/schemas/ChatResponse/additionalProperties",
        None,
    ),
    ("/components/schemas/ChatChunk/additionalProperties", None),
    ("/components/schemas/ChatChunkChoice/properties/delta", None),
    (
        "/components/schemas/ChatChunkChoice/additionalProperties",
        None,
    ),
    (
        "/components/schemas/EmbeddingRequest/additionalProperties",
        None,
    ),
    (
        "/components/schemas/EmbeddingResponse/properties/data/items",
        None,
    ),
    (
        "/components/schemas/EmbeddingResponse/additionalProperties",
        None,
    ),
    (
        "/components/schemas/RerankRequest/additionalProperties",
        None,
    ),
    (
        "/components/schemas/RerankResponse/properties/results/items",
        None,
    ),
    (
        "/components/schemas/RerankResponse/additionalProperties",
        None,
    ),
    (
        "/components/schemas/SpeechRequest/additionalProperties",
        None,
    ),
    (
        "/components/schemas/VerboseJsonTranscription/properties/words/items",
        Some("stt"),
    ),
];

/// Extension fields whose schema is a `$ref`, where utoipa drops field
/// extensions, or a list, where it moves them onto the items. Each gets
/// `x-runvil-extension: true` and `x-runvil-opt-in: false`, as the scalar
/// fields beside them get from their attributes.
const EXTENSION_FIELDS: &[&str] = &[
    "/components/schemas/ModelInfo/allOf/1/properties/kind",
    "/components/schemas/ModelInfo/allOf/1/properties/thinking",
    "/components/schemas/Capabilities/properties/effort_levels",
    "/components/schemas/Capabilities/properties/voices",
];

/// Untagged enums whose branches overlap, so a valid value matches more
/// than one: every inference model is also a valid speech catalog entry,
/// and every `verbose_json` reply is also a valid `json` reply. Their
/// `oneOf` becomes `anyOf`.
const OVERLAPPING_UNIONS: &[(&str, Option<&str>)] = &[
    ("CatalogModelInfo", None),
    ("TranscriptionResponse", Some("stt")),
];

/// Components that exist only in a feature build, beyond those reached
/// only from that feature's operations: `x-runvil-feature`.
const FEATURE_COMPONENTS: &[(&str, &str)] = &[("SpeechCatalogModelInfo", "stt")];

/// The JSON shapes a server-sent event stream's `data:` lines carry:
/// `x-runvil-event-schemas`, naming components.
const EVENT_SCHEMAS: &[(&str, &str, &str, &[&str])] = &[(
    "/v1/chat/completions",
    "post",
    "200",
    &["ChatChunk", "ErrorEnvelope"],
)];

/// One response's error codes: its status key and the codes.
type StatusCodes = (&'static str, &'static [&'static str]);

/// One operation's error codes: its path, its method, the feature that
/// mounts it, and its responses.
type OperationCodes = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static [StatusCodes],
);

const AUTH: &[&str] = &["unauthorized"];
const MALFORMED: &[&str] = &["malformed_request"];
const UPSTREAM: &[&str] = &[
    "upstream_connect",
    "upstream_error",
    "upstream_protocol",
    "upstream_transport",
];
const PASSED_THROUGH: &[&str] = &["upstream_client_error"];
const CACHE_FAILURE: &[&str] = &["blocking_task_failed", "cache_error"];

/// The error codes of a routed relay: chat, embeddings, and rerank.
const ROUTED: &[StatusCodes] = &[
    (
        "400",
        &["kind_mismatch", "malformed_request", "model_unavailable"],
    ),
    ("401", AUTH),
    ("404", &["model_not_found"]),
    ("429", &["queue_rejected"]),
    ("4XX", PASSED_THROUGH),
    ("502", UPSTREAM),
    (
        "503",
        &["model_loading", "model_provisioning", "queue_full"],
    ),
];

/// The machine-readable `error.code` values each JSON error response can
/// carry: `x-runvil-error-codes`, per path, method, and status. The codes
/// are the ones `GatewayError::classify`, `ProtocolError::classify`, and
/// the speech crate's batch error mapping write.
const ERROR_CODES: &[OperationCodes] = &[
    ("/v1/chat/completions", "post", None, ROUTED),
    ("/v1/embeddings", "post", None, ROUTED),
    ("/v1/rerank", "post", None, ROUTED),
    (
        "/v1/audio/speech",
        "post",
        None,
        &[
            (
                "400",
                &[
                    "invalid_voice",
                    "kind_mismatch",
                    "malformed_request",
                    "model_unavailable",
                ],
            ),
            ("401", AUTH),
            ("404", &["model_not_found"]),
            ("429", &["queue_rejected", "upstream_rate_limited"]),
            ("4XX", PASSED_THROUGH),
            ("502", UPSTREAM),
            (
                "503",
                &[
                    "model_loading",
                    "model_provisioning",
                    "queue_full",
                    "upstream_unavailable",
                ],
            ),
        ],
    ),
    ("/v1/audio/voices", "get", None, &[("401", AUTH)]),
    ("/v1/models", "get", None, &[("401", AUTH)]),
    (
        "/v1/tools/web_search",
        "post",
        Some("web-search"),
        &[
            ("400", MALFORMED),
            ("401", AUTH),
            ("404", &["not_found"]),
            ("4XX", PASSED_THROUGH),
            ("502", UPSTREAM),
        ],
    ),
    (
        "/v1/cache",
        "get",
        Some("local"),
        &[("401", AUTH), ("500", CACHE_FAILURE)],
    ),
    (
        "/v1/cache",
        "post",
        Some("local"),
        &[("400", MALFORMED), ("401", AUTH), ("500", CACHE_FAILURE)],
    ),
    (
        "/v1/cache/{sha256}",
        "delete",
        Some("local"),
        &[
            ("400", MALFORMED),
            ("401", AUTH),
            ("404", &["cache_entry_not_found"]),
            ("500", CACHE_FAILURE),
        ],
    ),
    (
        "/v1/audio/transcriptions",
        "post",
        Some("stt"),
        &[
            ("400", MALFORMED),
            ("401", AUTH),
            ("404", &["model_not_found"]),
            ("413", &["file_too_large"]),
            ("500", &["transcription_error"]),
        ],
    ),
    ("/v1/realtime", "get", Some("stt"), &[("401", AUTH)]),
];

/// Adds every marker in the tables to `document`.
///
/// # Errors
/// Returns one line per row this build should mark whose pointer does not
/// resolve to an object.
pub(super) fn apply(document: &mut Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for (name, feature) in OVERLAPPING_UNIONS {
        if feature.is_some_and(|feature| !enabled(feature)) {
            continue;
        }
        let pointer = format!("/components/schemas/{name}");
        let union = document
            .pointer_mut(&pointer)
            .and_then(Value::as_object_mut);
        match union.and_then(|union| union.remove("oneOf").map(|branches| (union, branches))) {
            Some((union, branches)) => {
                union.insert("anyOf".to_owned(), branches);
            }
            None => errors.push(format!("{pointer}: no `oneOf` to widen to `anyOf`")),
        }
    }
    let mut put = |pointer: &str, feature: Option<&str>, key: &str, value: Value| {
        if feature.is_some_and(|feature| !enabled(feature)) {
            return;
        }
        match document.pointer_mut(pointer).and_then(Value::as_object_mut) {
            Some(object) => {
                object.insert(key.to_owned(), value);
            }
            None => errors.push(format!("{pointer}: no object to mark with `{key}`")),
        }
    };
    for (pointer, feature) in PASS_THROUGH {
        put(pointer, *feature, "x-runvil-pass-through", json!(true));
    }
    for pointer in EXTENSION_FIELDS {
        put(pointer, None, "x-runvil-extension", json!(true));
        put(pointer, None, "x-runvil-opt-in", json!(false));
    }
    for (name, feature) in FEATURE_COMPONENTS {
        let pointer = format!("/components/schemas/{name}");
        put(&pointer, Some(feature), "x-runvil-feature", json!(feature));
    }
    for (path, method, status, names) in EVENT_SCHEMAS {
        let pointer = format!(
            "{}/content/text~1event-stream",
            response_pointer(path, method, status)
        );
        put(&pointer, None, "x-runvil-event-schemas", json!(names));
    }
    for (path, method, feature, statuses) in ERROR_CODES {
        for (status, codes) in *statuses {
            let pointer = response_pointer(path, method, status);
            put(&pointer, *feature, "x-runvil-error-codes", json!(codes));
            // `model_loading` is the one error that sends `Retry-After`,
            // always 5 seconds; utoipa has no `const` to say so.
            if codes.contains(&"model_loading") {
                let header = format!("{pointer}/headers/Retry-After");
                let schema = json!({ "const": 5, "type": "integer" });
                put(&header, *feature, "schema", schema);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The JSON pointer of one response object.
fn response_pointer(path: &str, method: &str, status: &str) -> String {
    let path = path.replace('~', "~0").replace('/', "~1");
    format!("/paths/{path}/{method}/responses/{status}")
}
