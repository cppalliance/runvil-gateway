//! Every documented operation and component, and the source line it comes from.
//!
//! `AUDIT_OPENAPI=1 cargo test -p gateway --lib openapi::audit -- --nocapture`
//! prints the map. The assertions run either way, so a renamed type fails here.

use super::source;
use super::tests::generated;

const COMPONENTS: &[(&str, &str, &str)] = &[
    (
        "CacheDeletedReply",
        "crates/gateway/app/src/cache.rs",
        "CacheDeletedReply",
    ),
    (
        "CacheDownloadEvent",
        "crates/gateway/app/src/cache.rs",
        "CacheDownloadEvent",
    ),
    (
        "CacheEntry",
        "crates/gateway/local/src/cache.rs",
        "CacheEntry",
    ),
    (
        "CacheErrorEvent",
        "crates/gateway/app/src/cache.rs",
        "CacheErrorEvent",
    ),
    (
        "CacheReadyReply",
        "crates/gateway/app/src/cache.rs",
        "CacheReadyReply",
    ),
    (
        "CacheRequest",
        "crates/gateway/app/src/cache.rs",
        "CacheRequest",
    ),
    (
        "CatalogModelInfo",
        "crates/gateway/app/src/models.rs",
        "CatalogModelInfo",
    ),
    (
        "CatalogModelsResponse",
        "crates/gateway/app/src/models.rs",
        "CatalogModelsResponse",
    ),
    (
        "ChatChunk",
        "crates/gateway/protocol/src/wire.rs",
        "ChatChunk",
    ),
    (
        "ChatChunkChoice",
        "crates/gateway/protocol/src/wire.rs",
        "ChatChunkChoice",
    ),
    (
        "ChatRequest",
        "crates/gateway/protocol/src/wire.rs",
        "ChatRequest",
    ),
    (
        "ChatResponse",
        "crates/gateway/protocol/src/wire.rs",
        "ChatResponse",
    ),
    (
        "EmbeddingInput",
        "crates/gateway/protocol/src/wire/embedding.rs",
        "EmbeddingInput",
    ),
    (
        "EmbeddingRequest",
        "crates/gateway/protocol/src/wire/embedding.rs",
        "EmbeddingRequest",
    ),
    (
        "EmbeddingResponse",
        "crates/gateway/protocol/src/wire/embedding.rs",
        "EmbeddingResponse",
    ),
    (
        "ErrorEnvelope",
        "crates/gateway/app/src/error.rs",
        "envelope",
    ),
    (
        "HealthReply",
        "crates/gateway/app/src/health.rs",
        "HealthReply",
    ),
    (
        "ModelInfo",
        "crates/gateway-api-types/src/metadata.rs",
        "ModelInfo",
    ),
    (
        "ModelKind",
        "crates/gateway-api-types/src/metadata.rs",
        "ModelKind",
    ),
    (
        "RerankRequest",
        "crates/gateway/protocol/src/wire/rerank.rs",
        "RerankRequest",
    ),
    (
        "RerankResponse",
        "crates/gateway/protocol/src/wire/rerank.rs",
        "RerankResponse",
    ),
    (
        "SpeechRequest",
        "crates/gateway/protocol/src/wire/speech.rs",
        "SpeechRequest",
    ),
    (
        "SpeechResponseFormat",
        "crates/gateway/protocol/src/wire/speech.rs",
        "SpeechResponseFormat",
    ),
    (
        "SpeechStreamFormat",
        "crates/gateway/protocol/src/wire/speech.rs",
        "SpeechStreamFormat",
    ),
    (
        "SpeechVoice",
        "crates/gateway/protocol/src/wire/speech.rs",
        "SpeechVoice",
    ),
    (
        "ThinkingMode",
        "crates/gateway-api-types/src/metadata.rs",
        "ThinkingMode",
    ),
    ("Voice", "crates/gateway/app/src/speech.rs", "Voice"),
    (
        "VoicesReply",
        "crates/gateway/app/src/speech.rs",
        "VoicesReply",
    ),
];

#[cfg(feature = "stt")]
const STT_COMPONENTS: &[(&str, &str, &str)] = &[
    (
        "JsonTranscription",
        "crates/gateway/stt/api/src/batch.rs",
        "JsonTranscription",
    ),
    (
        "SpeechCatalogModelInfo",
        "crates/gateway/app/src/models.rs",
        "SpeechCatalogModelInfo",
    ),
    (
        "TranscriptionResponse",
        "crates/gateway/stt/api/src/batch.rs",
        "TranscriptionResponse",
    ),
    (
        "TranscriptionSegment",
        "crates/gateway/stt/api/src/batch.rs",
        "TranscriptionSegment",
    ),
    (
        "VerboseJsonTranscription",
        "crates/gateway/stt/api/src/batch.rs",
        "VerboseJsonTranscription",
    ),
];

#[cfg(feature = "web-search")]
const WEB_COMPONENTS: &[(&str, &str, &str)] = &[
    (
        "SearchResult",
        "crates/gateway/web-search/src/service.rs",
        "SearchResult",
    ),
    (
        "WebSearchRequest",
        "crates/gateway/web-search/src/service.rs",
        "WebSearchRequest",
    ),
    (
        "WebSearchResponse",
        "crates/gateway/web-search/src/service.rs",
        "WebSearchResponse",
    ),
];

#[cfg(not(any(feature = "stt", feature = "web-search")))]
fn components() -> Vec<(&'static str, &'static str, &'static str)> {
    COMPONENTS.to_vec()
}

#[cfg(any(feature = "stt", feature = "web-search"))]
fn components() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut components = COMPONENTS.to_vec();
    #[cfg(feature = "stt")]
    components.extend(STT_COMPONENTS);
    #[cfg(feature = "web-search")]
    components.extend(WEB_COMPONENTS);
    components
}

#[test]
fn audit() {
    let document = generated();
    let print = std::env::var_os("AUDIT_OPENAPI").is_some();
    let mut lines = Vec::new();
    let paths = document["paths"].as_object().expect("paths");
    for (path, item) in paths {
        let item = item.as_object().expect("a path item");
        for (method, operation) in item {
            let source_ref = operation["x-runvil-source"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no x-runvil-source"));
            let (file, item_name) = source_ref
                .split_once('#')
                .unwrap_or_else(|| panic!("{source_ref} is not file#item"));
            let line = source::item_line(file, item_name).unwrap_or_else(|error| panic!("{error}"));
            lines.push(format!("{method} {path}  {file}:{line}  {item_name}"));
            let responses = operation["responses"].as_object().expect("responses");
            for (status, response) in responses {
                for source_ref in sources(&response["x-runvil-source"]) {
                    let (file, item_name) = source_ref
                        .split_once('#')
                        .unwrap_or_else(|| panic!("{source_ref} is not file#item"));
                    let line = source::item_line(file, item_name)
                        .unwrap_or_else(|error| panic!("{method} {path} {status}: {error}"));
                    lines.push(format!(
                        "{method} {path} {status}  {file}:{line}  {item_name}"
                    ));
                }
            }
        }
    }
    let schemas = document["components"]["schemas"]
        .as_object()
        .expect("schemas");
    let components = components();
    for (name, file, item) in &components {
        assert!(
            schemas.contains_key(*name),
            "audit map names `{name}`, which the document does not"
        );
        let line = source::item_line(file, item).unwrap_or_else(|error| panic!("{error}"));
        lines.push(format!("schema {name}  {file}:{line}  {item}"));
    }
    for name in schemas.keys() {
        assert!(
            components.iter().any(|(component, _, _)| component == name),
            "component `{name}` has no audit entry"
        );
    }
    if print {
        for line in &lines {
            println!("{line}");
        }
    }
}

/// A response's `x-runvil-source`: one string, a list after a merge, or none.
fn sources(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(single) => vec![single.as_str()],
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().expect("a source is a string"))
            .collect(),
        _ => Vec::new(),
    }
}
