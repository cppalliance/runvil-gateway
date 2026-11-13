//! The schema functions the operation table points at. Each one asks the
//! caller's generator, so a request and a response keep their own contract.

use schemars::JsonSchema;
use schemars::generate::SchemaGenerator;
use serde_json::Value;

use crate::cache::{
    CacheDeletedReply, CacheDownloadEvent, CacheErrorEvent, CacheReadyReply, CacheRequest,
};
use crate::health::HealthReply;
use crate::models::CatalogModelsResponse;
use crate::speech::VoicesReply;
use crate::wire::{
    ChatChunk, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse, RerankRequest,
    RerankResponse, SpeechRequest,
};

/// The speech response is opaque bytes. The schema names that, and the
/// media type the handler actually sends is described beside it.
pub(super) fn audio_schema(_generator: &mut SchemaGenerator) -> Value {
    serde_json::json!({ "type": "string" })
}

fn schema<T: JsonSchema + ?Sized>(generator: &mut SchemaGenerator) -> Value {
    generator.subschema_for::<T>().to_value()
}

pub(super) fn chat_request(generator: &mut SchemaGenerator) -> Value {
    schema::<ChatRequest>(generator)
}

pub(super) fn chat_response(generator: &mut SchemaGenerator) -> Value {
    schema::<ChatResponse>(generator)
}

/// Registers the chat stream's chunk schema and describes the SSE body as text.
pub(super) fn chat_events(generator: &mut SchemaGenerator) -> Value {
    let _ = schema::<ChatChunk>(generator);
    serde_json::json!({ "type": "string" })
}

/// Registers every cache stream event and describes the SSE body as text.
pub(super) fn cache_events(generator: &mut SchemaGenerator) -> Value {
    let _ = schema::<CacheDownloadEvent>(generator);
    let _ = schema::<CacheReadyReply>(generator);
    let _ = schema::<CacheErrorEvent>(generator);
    serde_json::json!({ "type": "string" })
}

pub(super) fn embedding_request(generator: &mut SchemaGenerator) -> Value {
    schema::<EmbeddingRequest>(generator)
}

pub(super) fn embedding_response(generator: &mut SchemaGenerator) -> Value {
    schema::<EmbeddingResponse>(generator)
}

pub(super) fn rerank_request(generator: &mut SchemaGenerator) -> Value {
    schema::<RerankRequest>(generator)
}

pub(super) fn rerank_response(generator: &mut SchemaGenerator) -> Value {
    schema::<RerankResponse>(generator)
}

pub(super) fn speech_request(generator: &mut SchemaGenerator) -> Value {
    schema::<SpeechRequest>(generator)
}

pub(super) fn voices(generator: &mut SchemaGenerator) -> Value {
    schema::<VoicesReply>(generator)
}

pub(super) fn models(generator: &mut SchemaGenerator) -> Value {
    schema::<CatalogModelsResponse>(generator)
}

pub(super) fn health(generator: &mut SchemaGenerator) -> Value {
    schema::<HealthReply>(generator)
}

pub(super) fn cache_request(generator: &mut SchemaGenerator) -> Value {
    schema::<CacheRequest>(generator)
}

pub(super) fn cache_list(generator: &mut SchemaGenerator) -> Value {
    schema::<Vec<gateway_local::cache::CacheEntry>>(generator)
}

pub(super) fn cache_deleted(generator: &mut SchemaGenerator) -> Value {
    schema::<CacheDeletedReply>(generator)
}

pub(super) fn cache_ready(generator: &mut SchemaGenerator) -> Value {
    schema::<CacheReadyReply>(generator)
}

#[cfg(feature = "web-search")]
pub(super) fn web_search_request(generator: &mut SchemaGenerator) -> Value {
    schema::<gateway_web_search::WebSearchRequest>(generator)
}

#[cfg(feature = "web-search")]
pub(super) fn web_search_response(generator: &mut SchemaGenerator) -> Value {
    schema::<gateway_web_search::WebSearchResponse>(generator)
}

#[cfg(feature = "stt")]
pub(super) fn transcription(generator: &mut SchemaGenerator) -> Value {
    gateway_stt::transcription_response_schema(generator).to_value()
}
