//! Routing table entries: the [`Model`] and [`Endpoint`] vocabulary shared by
//! the gateway's routing table and the local inference crate.

use std::fmt;
use std::sync::Arc;

use gateway_config::{Capabilities, ModelKind, ThinkingMode};
use gateway_protocol::upstream::Upstream;

use crate::queue::DominionQueue;

/// The `tool_dialect` value selecting the emulated Gemma3 `tool_code`
/// content-fence dialect.
///
/// This is vocabulary for [`Model::tool_dialect`]: the local inference
/// crate's dialect probing resolves it from child evidence, and the gateway's
/// dialect emulation matches on it, so both sides name one constant.
pub const GEMMA3_TOOL_CODE: &str = "gemma3_tool_code";

/// Where an endpoint's credential comes from. A request is authorized by
/// this, never by a model's or an endpoint's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// The endpoint's own configuration, or none for a local model.
    Configured,
    /// A user's ChatGPT sign-in, held by the gateway. The catalog entry's
    /// `upstream` spends nothing; the gateway hands the one that does only
    /// to a request it has authorized.
    ChatgptPlan,
}

/// One backend endpoint plus the upstream that talks to it.
pub struct Endpoint {
    /// The endpoint's configured id.
    pub id: String,
    /// Where this endpoint's credential comes from.
    pub source: CredentialSource,
    /// The upstream implementation forwarding to this backend.
    pub upstream: Arc<dyn Upstream>,
    /// Admission control: concurrency limit plus bounded waiting queue.
    /// Endpoints bound to the same dominion hold clones of one shared queue
    /// and compete for a single pool of slots; an endpoint with no dominion
    /// is unlimited.
    pub queue: DominionQueue,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("queue", &self.queue)
            .finish_non_exhaustive()
    }
}

/// One model, resolved to a backend endpoint and the backend's model string.
#[derive(Debug)]
pub struct Model {
    /// The caller-facing model name.
    pub name: String,
    /// The workload this model serves: chat, embedding, classifier, or
    /// speech.
    pub kind: ModelKind,
    /// Prose describing the model for catalog consumers.
    pub description: String,
    /// Context window size in tokens.
    pub context: u32,
    /// Whether thinking tokens are never, always, or switchably available.
    pub thinking: ThinkingMode,
    /// Capability metadata advertised on the catalog.
    pub capabilities: Capabilities,
    /// The tool-calling dialect used by this model (e.g. `"openai"`, `"gemma3_tool_code"`).
    pub tool_dialect: String,
    /// The string the backend knows this model by.
    pub upstream_name: String,
    /// The endpoint serving this model: the first configured one.
    pub endpoint: Arc<Endpoint>,
}

impl Model {
    /// This model, sent through `upstream` in place of its endpoint's own.
    /// Everything else, the endpoint's queue included, is unchanged.
    #[must_use]
    pub fn with_upstream(&self, upstream: Arc<dyn Upstream>) -> Model {
        Model {
            name: self.name.clone(),
            kind: self.kind,
            description: self.description.clone(),
            context: self.context,
            thinking: self.thinking,
            capabilities: self.capabilities.clone(),
            tool_dialect: self.tool_dialect.clone(),
            upstream_name: self.upstream_name.clone(),
            endpoint: Arc::new(Endpoint {
                id: self.endpoint.id.clone(),
                source: self.endpoint.source,
                upstream,
                queue: self.endpoint.queue.clone(),
            }),
        }
    }
}
