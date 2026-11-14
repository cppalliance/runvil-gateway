//! Closed-vocabulary enums for the keyword-valued configuration fields.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The wire protocol an endpoint speaks upstream. Callers always speak chat
/// completions to the gateway; this names what the gateway speaks to the
/// backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Protocol {
    /// The OpenAI `/chat/completions` shape, forwarded unchanged.
    Openai,
    /// The OpenAI `/responses` shape. The gateway translates each chat
    /// completion to a Responses request and the Responses event stream
    /// back to chat completion chunks, so callers see no difference.
    #[serde(rename = "openai-responses")]
    OpenaiResponses,
}

/// Where an endpoint's credential comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EndpointAuth {
    /// The endpoint's own `api_key`. The default when `auth` is absent.
    #[default]
    #[serde(rename = "api-key")]
    ApiKey,
    /// A user's ChatGPT sign-in, held by the gateway. The endpoint sets
    /// neither `base_url` nor `api_key`: the gateway sends the plan's
    /// credential to OpenAI's API and nowhere else, and only for Runvil's
    /// own client.
    #[serde(rename = "chatgpt-plan")]
    ChatgptPlan,
}

impl EndpointAuth {
    /// Whether this is the default, which a serialized endpoint omits.
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's skip_serializing_if passes a reference"
    )]
    pub(crate) fn is_api_key(&self) -> bool {
        *self == EndpointAuth::ApiKey
    }
}

/// Whether a dominion pools remote providers or local GPUs managed by the
/// gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DominionKind {
    /// A pool of remote HTTP providers, bindable by `[[endpoint]]` entries.
    Remote,
    /// A local GPU, bindable by `[[local_model]]` entries.
    Local,
}

/// What a dominion's admission queue does when it is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum QueuePolicy {
    /// Waits for a slot up to `max_queue` waiting requests, then rejects.
    #[default]
    Queue,
    /// Rejects immediately when no concurrency slot is free (fail-fast).
    Reject,
}

/// The `llama-server` build the gateway downloads for local inference on
/// Windows x86-64. Every other platform has exactly one build, so this
/// setting is consulted there only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LlamaBackend {
    /// Picks from the machine's GPUs: a Blackwell (compute capability 12.x) gets
    /// the Runvil CUDA build, any other NVIDIA GPU gets the upstream
    /// CUDA build, and anything else gets Vulkan.
    #[default]
    Auto,
    /// The Runvil Blackwell build (`llama-cuda-blackwell` release).
    CudaBlackwell,
    /// The upstream llama.cpp CUDA 13 build.
    Cuda,
    /// The upstream llama.cpp Vulkan build.
    Vulkan,
}

impl LlamaBackend {
    /// True for the default (`auto`), so serialization can omit it.
    #[must_use]
    pub fn is_auto(&self) -> bool {
        *self == LlamaBackend::Auto
    }
}

/// The tool-calling dialect a chat model speaks.
///
/// `openai` (the default) forwards tool definitions verbatim and expects
/// native wire `tool_calls`. `gemma3_tool_code` emulates tool calling for
/// backends without a native tool array: the gateway injects a tool guide
/// into the system prompt, strips `tools`/`tool_choice` from the outgoing
/// request, and parses `tool_code` content fences from the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolDialect {
    /// Native OpenAI tool calling. The default.
    #[default]
    Openai,
    /// Emulated Gemma3 `tool_code` content-fence protocol.
    Gemma3ToolCode,
}

impl fmt::Display for ToolDialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spelling = match self {
            ToolDialect::Openai => "openai",
            ToolDialect::Gemma3ToolCode => "gemma3_tool_code",
        };
        f.write_str(spelling)
    }
}

/// A web-search provider. Brave is the only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SearchProvider {
    /// The Brave Search API.
    Brave,
}
