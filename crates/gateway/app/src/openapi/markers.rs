//! The `x-runvil-*` markers utoipa's attributes cannot carry, added to the
//! serialized document at fixed JSON pointers.
//!
//! utoipa takes extensions on operations, parameters, and named schema
//! fields of scalar type. It has no place for them on a response, on a
//! media type, on a whole component, on a field whose schema is a `$ref`,
//! or on a schema that accepts any value, and on a list field it puts them
//! on the items. It also writes every untagged enum as `oneOf`, which
//! rejects a value that matches more than one branch. Each table here
//! covers one of those gaps. It has no `const` either, so the error codes
//! table also pins the loading 503's `Retry-After` to 5. A row whose
//! feature this build leaves out is skipped; any other row whose pointer
//! does not resolve fails generation.

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
