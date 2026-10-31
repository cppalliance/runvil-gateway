//! The Gateway's public wire vocabulary (`gateway-api-types`): the provider
//! model sheet schema, one versioned JSON snapshot of every provider's models
//! published as a release artifact and consumed by the Gateway and by the
//! separate repository that publishes it; the model metadata types; and the
//! [`Progress`] snapshot the Gateway streams to its status consumers.
//!
//! This crate is pure vocabulary: besides `serde` and `time` it depends only
//! on `schemars`, which derives the committed JSON Schema of the sheet, and
//! on no other workspace crate, so every product crate may depend on it.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

mod metadata;
pub mod progress;

pub use metadata::{Capabilities, ModelInfo, ModelKind, ThinkingMode};
pub use progress::Progress;

/// The sheet schema version this reader accepts: the publisher stamps it and
/// the gateway reader gates on it. Bump only on removals and renames;
/// additive fields are marked `#[serde(default)]` so a lagging reader
/// survives them.
pub const ACCEPTED_SHEET_SCHEMA_VERSION: u32 = 1;

/// The schema of `Sheet::generated_at`: one RFC 3339 instant, the shape
/// `time::serde::rfc3339` writes.
///
/// `schemars` has no `time` support, and it reads `#[serde(with = "...")]`
/// as a type path, so the shape is stated here instead.
fn generated_at_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({ "type": "string", "format": "date-time" })
}

/// The schema of `ProviderSlice::fetched_at`: an RFC 3339 instant or null.
/// The key is always written, so it stays required and only the value is
/// nullable.
fn fetched_at_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [
            { "type": "string", "format": "date-time" },
            { "type": "null" }
        ]
    })
}

/// The schema of `ModelEntry::released_at`: a calendar date or null, the
/// shape `time`'s `serde-human-readable` rendering gives a `Date`, as in
/// `"2026-07-24"`.
fn released_at_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [
            { "type": "string", "format": "date" },
            { "type": "null" }
        ]
    })
}

/// The schema of `Deprecation::date`: a calendar date or null, in the same
/// rendering as [`released_at_schema`].
fn deprecation_date_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "anyOf": [
            { "type": "string", "format": "date" },
            { "type": "null" }
        ]
    })
}

/// The sheet envelope: one atomic snapshot of every provider's models.
/// Future additive fields are marked `#[serde(default)]` so a lagging
/// reader survives them; `schema_version` bumps only on removals and
/// renames.
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct Sheet {
    /// Bumped on breaking change. Stays 1: additive fields are marked
    /// `#[serde(default)]`; the bump is reserved for removals and renames.
    //
    // The `1` is a literal on purpose. `extend` emits its token stream as a
    // JSON value, so a path to the constant would not be evaluated; the
    // schema test fails if this literal and the constant ever disagree.
    #[schemars(extend("const" = 1))]
    pub schema_version: u32,
    /// RFC 3339; always this run's time.
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(schema_with = "generated_at_schema")]
    pub generated_at: OffsetDateTime,
    /// Keyed by provider name, e.g. "anthropic".
    pub providers: BTreeMap<String, ProviderSlice>,
}

/// One provider's slice of the sheet. Self-describing: the descriptor's
/// public fields are copied in at build time so consumers can render a
/// provider dropdown from the sheet alone. Future additive fields are
/// marked `#[serde(default)]`; the schema version bump is reserved for
/// removals and renames.
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct ProviderSlice {
    /// UI-facing name, e.g. "Anthropic".
    pub display_name: String,
    /// The provider's tier, a curated product opinion.
    pub tier: Tier,
    /// Freshness of this slice.
    pub status: SliceStatus,
    /// Last fresh fetch; absent for `static` slices.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schemars(schema_with = "fetched_at_schema")]
    pub fetched_at: Option<OffsetDateTime>,
    /// The base URL of the provider's OpenAI-compatible chat API - the
    /// value an `[[endpoint]]` needs - or `None` when the provider has
    /// no such API.
    pub openai_base_url: Option<String>,
    /// The provider's environment variables, copied from the descriptor
    /// at build time so a consumer can render from the sheet alone.
    pub env_vars: Vec<EnvVar>,
    /// The provider's normalized model entries.
    pub models: Vec<ModelEntry>,
}

/// One environment variable a provider reads.
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct EnvVar {
    /// The variable name, e.g. "ANTHROPIC_API_KEY".
    pub name: String,
    /// How the variable is used.
    pub role: EnvRole,
    /// The value the provider assumes when the variable is unset.
    pub default: Option<String>,
}

/// How a provider uses an environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EnvRole {
    /// A credential: the variable holds API key material.
    Key,
    /// Configuration: the variable selects a region, endpoint, or similar.
    Config,
}

/// Curated product opinion, not a vendor fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Tier {
    /// The frontier providers.
    Prime,
    /// Credible challengers.
    Subprime,
    /// Specialized or regional providers.
    Niche,
    /// Resellers of other providers' models.
    Aggregator,
}

/// Freshness of one provider's slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SliceStatus {
    /// Fetched fresh this run.
    Ok,
    /// Copied verbatim from the previous sheet after a failed fetch.
    Stale,
    /// No previous slice and the fetch failed; `models` is empty.
    Unavailable,
    /// Curated by hand; never fetched.
    Static,
}

/// One normalized model entry. Future additive fields are marked
/// `#[serde(default)]`; the schema version bump is reserved for removals
/// and renames.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the modality and capability booleans are the sheet schema itself; a builder or sub-struct would only obscure the wire shape"
)]
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct ModelEntry {
    /// The upstream slug.
    pub id: String,
    /// UI-facing name.
    pub display_name: String,
    /// The UI's first grouping level, e.g. "claude-opus", "gpt-5.4".
    pub family: String,
    /// The canonical alias this entry is a dated snapshot or SKU variant
    /// of; set only when that alias exists in the same provider's list.
    pub variant_of: Option<String>,
    /// The stripped suffix, e.g. `2026-03-05`, `free`, `batch`, for the
    /// UI's expander label.
    pub variant: Option<String>,
    /// The provider's own language codes, passed through as reported;
    /// empty when unknown or not applicable.
    pub languages: Vec<String>,
    /// The workload: chat, embedding, classifier, speech (TTS),
    /// transcription (STT), image, video.
    #[serde(default)]
    pub kind: ModelKind,
    /// Optional: not every provider reports it. Absent reads as `None`, so
    /// the field is not required; the writer still emits the key.
    #[serde(default)]
    #[schemars(schema_with = "released_at_schema")]
    pub released_at: Option<Date>,
    /// Optional: the IDs-only providers omit it.
    pub context_window: Option<u32>,
    /// Optional maximum completion tokens.
    pub max_output: Option<u32>,
    // Modalities.
    /// Accepts image input.
    pub images: bool,
    /// Accepts PDF input.
    pub pdf_input: bool,
    /// Accepts video input.
    pub video_input: bool,
    /// Accepts audio input.
    pub audio_input: bool,
    // Capabilities.
    /// Supports batch submission.
    pub batch: bool,
    /// Returns grounded citations.
    pub citations: bool,
    /// Can execute code server-side.
    pub code_execution: bool,
    /// Honors response schemas.
    pub structured_outputs: bool,
    /// Emits tool calls.
    pub tool_calling: bool,
    /// Reasoning capability.
    pub thinking: Thinking,
    /// The provider's own level names, e.g. `["low", "high", "max"]`;
    /// never mapped to a cross-provider scale.
    pub effort_levels: Vec<String>,
    /// The provider's own default level name.
    pub default_effort: Option<String>,
    /// Normalized to per-million-token units.
    pub pricing: Option<Pricing>,
    /// Sunset information, when the provider reports it.
    pub deprecation: Option<Deprecation>,
}

/// Reasoning capability.
#[derive(Debug, Clone, Copy, Default, JsonSchema, Serialize, Deserialize)]
pub struct Thinking {
    /// Any reasoning capability at all.
    pub supported: bool,
    /// Manual budget mode (Anthropic "enabled").
    pub enabled: bool,
    /// Model-chosen thinking depth.
    pub adaptive: bool,
}

/// Token pricing, normalized to per-million-token units.
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct Pricing {
    /// ISO 4217, e.g. "USD", "CNY".
    pub currency: String,
    /// Prompt price per million tokens.
    pub prompt_per_mtok: f64,
    /// Completion price per million tokens.
    pub completion_per_mtok: f64,
}

/// Sunset information, when the provider reports it.
#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
pub struct Deprecation {
    /// Provider's own lifecycle label, e.g. "LEGACY".
    pub status: String,
    /// The sunset date, when known. Absent reads as `None`, so the field is
    /// not required; the writer still emits the key.
    #[serde(default)]
    #[schemars(schema_with = "deprecation_date_schema")]
    pub date: Option<Date>,
    /// The successor model id, when named.
    pub replacement: Option<String>,
}

#[cfg(test)]
#[path = "lib-tests.rs"]
mod tests;
