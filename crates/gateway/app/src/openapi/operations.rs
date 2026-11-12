//! One entry per in-scope operation. The registry coverage test fails when
//! this list and the mounted routes disagree.

use axum::http::Method;
use schemars::generate::SchemaGenerator;
use serde_json::Value;

use super::errors::Variant;
use super::fields::{ExtensionField, FieldDoc};
use super::{schemas, success};

/// Which generator a schema is built with.
#[derive(Clone, Copy)]
pub(super) enum Contract {
    /// The deserialize contract: what a request accepts.
    Request,
    /// The serialize contract: what a response emits.
    Response,
}

/// One media type on a request or a response.
pub(super) struct Content {
    /// The media type.
    pub media: &'static str,
    /// Builds the schema into the generator of `contract`.
    pub schema: fn(&mut SchemaGenerator) -> Value,
    /// Which contract `schema` belongs to.
    pub contract: Contract,
    /// Doc of the function that emits this body, when it is not the handler.
    pub doc: Option<(&'static str, &'static str)>,
    /// Component names of the JSON events inside an SSE body.
    pub events: &'static [&'static str],
    /// The body is `multipart/form-data`, read from `parse_form`'s arms and
    /// described by its doc's paragraph for each field.
    pub multipart: bool,
}

/// One documented operation.
pub(super) struct Operation {
    /// The registry path.
    pub path: &'static str,
    /// The method.
    pub method: Method,
    /// The `operationId`.
    pub id: &'static str,
    /// The file the handler lives in, relative to the workspace.
    pub file: &'static str,
    /// The handler function.
    pub handler: &'static str,
    /// The tag, which is the area.
    pub tag: &'static str,
    /// `x-runvil-standard` when the route follows one.
    pub standard: Option<&'static str>,
    /// `x-runvil-feature` when a feature compiles the route in.
    pub feature: Option<&'static str>,
    /// Whether `check_auth` runs.
    pub authed: bool,
    /// Whether the handler reads `X-Runvil-Client`.
    pub client_header: bool,
    /// The realtime upgrade, which OpenAPI cannot describe past the handshake.
    pub websocket: bool,
    /// Extension fields that live inside a pass-through value.
    pub extension_fields: &'static [ExtensionField],
    /// The request body, when the route has one.
    pub body: Option<Content>,
    /// The success response's media types.
    pub success: Vec<Content>,
    /// The success status.
    pub success_status: u16,
    /// The failures the handler can return.
    pub errors: &'static [Variant],
    /// A path capture, described by the paragraph of the handler's doc that
    /// opens with its name.
    pub path_param: Option<&'static str>,
}

const RELAY: &str = "crates/gateway/app/src/relay.rs";
const SPEECH: &str = "crates/gateway/app/src/speech.rs";
const MODELS: &str = "crates/gateway/app/src/models.rs";
const HEALTH: &str = "crates/gateway/app/src/health.rs";
#[cfg(feature = "web-search")]
const WEB: &str = "crates/gateway/app/src/web_search.rs";
const CACHE: &str = "crates/gateway/app/src/cache.rs";
#[cfg(feature = "stt")]
const BATCH: &str = "crates/gateway/stt/api/src/batch.rs";
#[cfg(feature = "stt")]
const REALTIME: &str = "crates/gateway/stt/api/src/realtime/route.rs";

const ROUTED: &[Variant] = &[
    Variant::Malformed,
    Variant::UnknownModel,
    Variant::KindMismatch,
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
];

const CHAT: &[Variant] = ROUTED;

const SPEECH_ERRORS: &[Variant] = &[
    Variant::Malformed,
    Variant::InvalidVoice,
    Variant::UnknownModel,
    Variant::KindMismatch,
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
    Variant::UpstreamRateLimited,
    Variant::UpstreamUnavailable,
];

fn json_request(schema: fn(&mut SchemaGenerator) -> Value) -> Content {
    Content {
        media: "application/json",
        schema,
        contract: Contract::Request,
        doc: None,
        events: &[],
        multipart: false,
    }
}

fn json_response(schema: fn(&mut SchemaGenerator) -> Value) -> Content {
    Content {
        media: "application/json",
        schema,
        contract: Contract::Response,
        doc: None,
        events: &[],
        multipart: false,
    }
}

fn sse(
    schema: fn(&mut SchemaGenerator) -> Value,
    doc: (&'static str, &'static str),
    events: &'static [&'static str],
) -> Content {
    Content {
        media: "text/event-stream",
        schema,
        contract: Contract::Response,
        doc: Some(doc),
        events,
        multipart: false,
    }
}

/// Every in-scope operation in this build, or the first media type that could
/// not be read from the code.
pub(super) fn operations() -> Result<Vec<Operation>, String> {
    let mut operations = relay_operations();
    operations.extend(speech_operations()?);
    #[cfg(feature = "web-search")]
    operations.push(web_search_operation());
    #[cfg(feature = "local")]
    operations.extend(cache_operations());
    #[cfg(feature = "stt")]
    operations.extend(stt_operations());
    Ok(operations)
}

fn relay_operations() -> Vec<Operation> {
    vec![
        Operation {
            path: "/health",
            method: Method::GET,
            id: "get_health",
            file: HEALTH,
            handler: "health",
            tag: "health",
            standard: None,
            feature: None,
            authed: false,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: None,
            success: vec![json_response(schemas::health)],
            success_status: 200,
            errors: &[],
            path_param: None,
        },
        Operation {
            path: "/v1/chat/completions",
            method: Method::POST,
            id: "post_v1_chat_completions",
            file: RELAY,
            handler: "chat_completions",
            tag: "relay",
            standard: Some("openai"),
            feature: None,
            authed: true,
            client_header: true,
            websocket: false,
            extension_fields: &[ExtensionField {
                name: "choices[].message.gateway_warning",
                file: "crates/gateway/protocol/src/wire.rs",
                doc: FieldDoc::ModuleParagraph("gateway_warning"),
                opt_in: None,
            }],
            body: Some(json_request(schemas::chat_request)),
            success: vec![
                json_response(schemas::chat_response),
                sse(
                    schemas::chat_events,
                    (RELAY, "relay_sse"),
                    &["ChatChunk", "ErrorEnvelope"],
                ),
            ],
            success_status: 200,
            errors: CHAT,
            path_param: None,
        },
        Operation {
            path: "/v1/embeddings",
            method: Method::POST,
            id: "post_v1_embeddings",
            file: RELAY,
            handler: "embeddings",
            tag: "relay",
            standard: Some("openai"),
            feature: None,
            authed: true,
            client_header: true,
            websocket: false,
            extension_fields: &[],
            body: Some(json_request(schemas::embedding_request)),
            success: vec![json_response(schemas::embedding_response)],
            success_status: 200,
            errors: ROUTED,
            path_param: None,
        },
        Operation {
            path: "/v1/rerank",
            method: Method::POST,
            id: "post_v1_rerank",
            file: RELAY,
            handler: "rerank",
            tag: "relay",
            standard: Some("jina-cohere-rerank"),
            feature: None,
            authed: true,
            client_header: true,
            websocket: false,
            extension_fields: &[],
            body: Some(json_request(schemas::rerank_request)),
            success: vec![json_response(schemas::rerank_response)],
            success_status: 200,
            errors: ROUTED,
            path_param: None,
        },
    ]
}

fn speech_operations() -> Result<Vec<Operation>, String> {
    Ok(vec![
        Operation {
            path: "/v1/audio/speech",
            method: Method::POST,
            id: "post_v1_audio_speech",
            file: SPEECH,
            handler: "audio_speech",
            tag: "speech",
            standard: Some("openai"),
            feature: None,
            authed: true,
            client_header: true,
            websocket: false,
            extension_fields: &[],
            body: Some(json_request(schemas::speech_request)),
            success: success::speech_audio()?,
            success_status: 200,
            errors: SPEECH_ERRORS,
            path_param: None,
        },
        Operation {
            path: "/v1/audio/voices",
            method: Method::GET,
            id: "get_v1_audio_voices",
            file: SPEECH,
            handler: "audio_voices",
            tag: "speech",
            standard: None,
            feature: None,
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: None,
            success: vec![json_response(schemas::voices)],
            success_status: 200,
            errors: &[],
            path_param: None,
        },
        Operation {
            path: "/v1/models",
            method: Method::GET,
            id: "get_v1_models",
            file: MODELS,
            handler: "list_models",
            tag: "models",
            standard: Some("openai"),
            feature: None,
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: None,
            success: vec![json_response(schemas::models)],
            success_status: 200,
            errors: &[],
            path_param: None,
        },
    ])
}

#[cfg(feature = "web-search")]
fn web_search_operation() -> Operation {
    Operation {
        path: "/v1/tools/web_search",
        method: Method::POST,
        id: "post_v1_tools_web_search",
        file: WEB,
        handler: "web_search",
        tag: "web_search",
        standard: None,
        feature: Some("web-search"),
        authed: true,
        client_header: false,
        websocket: false,
        extension_fields: &[],
        body: Some(json_request(schemas::web_search_request)),
        success: vec![json_response(schemas::web_search_response)],
        success_status: 200,
        errors: &[
            Variant::Malformed,
            Variant::ToolNotConfigured,
            Variant::UpstreamTransport,
            Variant::UpstreamConnect,
            Variant::UpstreamProtocol,
            Variant::UpstreamServer,
            Variant::UpstreamClient,
        ],
        path_param: None,
    }
}

#[cfg(feature = "local")]
fn cache_operations() -> Vec<Operation> {
    vec![
        Operation {
            path: "/v1/cache",
            method: Method::GET,
            id: "get_v1_cache",
            file: CACHE,
            handler: "list_cache",
            tag: "cache",
            standard: None,
            feature: Some("local"),
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: None,
            success: vec![json_response(schemas::cache_list)],
            success_status: 200,
            errors: &[Variant::Cache, Variant::Blocking],
            path_param: None,
        },
        Operation {
            path: "/v1/cache",
            method: Method::POST,
            id: "post_v1_cache",
            file: CACHE,
            handler: "post_cache",
            tag: "cache",
            standard: None,
            feature: Some("local"),
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: Some(json_request(schemas::cache_request)),
            success: vec![
                json_response(schemas::cache_ready),
                sse(
                    schemas::cache_events,
                    (CACHE, "sse_response"),
                    &["CacheDownloadEvent", "CacheReadyReply", "CacheErrorEvent"],
                ),
            ],
            success_status: 200,
            errors: &[Variant::Malformed, Variant::Cache, Variant::Blocking],
            path_param: None,
        },
        Operation {
            path: "/v1/cache/{sha256}",
            method: Method::DELETE,
            id: "delete_v1_cache_sha256",
            file: CACHE,
            handler: "delete_cache",
            tag: "cache",
            standard: None,
            feature: Some("local"),
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: None,
            success: vec![json_response(schemas::cache_deleted)],
            success_status: 200,
            errors: &[
                Variant::Malformed,
                Variant::CacheMissing,
                Variant::Cache,
                Variant::Blocking,
            ],
            path_param: Some("sha256"),
        },
    ]
}

#[cfg(feature = "stt")]
fn stt_operations() -> Vec<Operation> {
    vec![
        Operation {
            path: "/v1/audio/transcriptions",
            method: Method::POST,
            id: "post_v1_audio_transcriptions",
            file: BATCH,
            handler: "handler",
            tag: "stt",
            standard: Some("openai"),
            feature: Some("stt"),
            authed: true,
            client_header: false,
            websocket: false,
            extension_fields: &[],
            body: Some(Content {
                media: "multipart/form-data",
                schema: schemas::transcription,
                contract: Contract::Response,
                doc: None,
                events: &[],
                multipart: true,
            }),
            success: vec![json_response(schemas::transcription)],
            success_status: 200,
            errors: &[],
            path_param: None,
        },
        Operation {
            path: "/v1/realtime",
            method: Method::GET,
            id: "get_v1_realtime",
            file: REALTIME,
            handler: "upgrade",
            tag: "stt",
            standard: Some("openai-realtime-transcription"),
            feature: Some("stt"),
            authed: true,
            client_header: false,
            websocket: true,
            extension_fields: &[ExtensionField {
                name: "conversation.item.input_audio_transcription.hypothesis",
                file: "crates/gateway/stt/api/src/realtime/wire/vocabulary.rs",
                doc: FieldDoc::Item("HypothesisInclude"),
                opt_in: Some("HypothesisInclude"),
            }],
            body: None,
            success: vec![],
            success_status: 101,
            errors: &[],
            path_param: None,
        },
    ]
}
