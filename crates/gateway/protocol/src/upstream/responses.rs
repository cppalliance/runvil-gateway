//! The `openai-responses` endpoint protocol: chat completions from the
//! caller, OpenAI Responses to the backend.
//!
//! [`ResponsesUpstream`] rewrites each chat completion as a Responses request
//! ([`crate::wire::to_responses_request`]) and always asks the backend to
//! stream. A streaming caller gets the Responses events translated to chat
//! chunks as they arrive; a non-streaming caller gets the same chunks folded
//! into one chat completion. Either way the caller sees the chat completions
//! shape only.

use std::sync::Arc;

use async_trait::async_trait;

use super::{BearerSource, StreamedChunks, Upstream};
use crate::error::ProtocolError;
use crate::wire::{ChatRequest, ChatResponse, RequestPolicy, to_responses_request};

mod events;
mod sse;

/// A backend that speaks the OpenAI Responses API, reached over HTTP.
#[derive(Debug)]
pub struct ResponsesUpstream {
    base_url: String,
    bearer: Arc<dyn BearerSource>,
    policy: RequestPolicy,
    /// Connect-timeout-only client that never follows a redirect: every
    /// request streams, so a whole-request timeout would cut a long reply
    /// off, and a followed redirect could carry the bearer or the prompt to
    /// another server. `None` when hardened construction fails so calls fail
    /// closed before requesting a credential or contacting an upstream.
    http: Option<reqwest::Client>,
}

impl ResponsesUpstream {
    /// Builds an upstream for `base_url` (a trailing slash is trimmed) that
    /// presents `bearer` and sends only the fields `policy` allows.
    #[must_use]
    pub fn new(
        base_url: &str,
        bearer: Arc<dyn BearerSource>,
        policy: RequestPolicy,
    ) -> ResponsesUpstream {
        ResponsesUpstream {
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer,
            policy,
            http: crate::http_util::no_redirect_streaming_client(),
        }
    }

    /// Test-only constructor modeling a failed hardened HTTP client build.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn without_http_client(
        base_url: &str,
        bearer: Arc<dyn BearerSource>,
        policy: RequestPolicy,
    ) -> ResponsesUpstream {
        ResponsesUpstream {
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer,
            policy,
            http: None,
        }
    }

    /// Translates `req`, sends it to `{base_url}/responses`, and returns the
    /// opened event stream.
    ///
    /// # Errors
    /// Returns [`ProtocolError::UnsupportedRequest`] before anything is sent
    /// when the request has no Responses equivalent, [`ProtocolError::UpstreamConnect`]
    /// before touching the credential or network when no hardened HTTP client
    /// is available, the bearer source's error when it has no credential, and
    /// the transport and status errors of [`super::Upstream::stream`].
    async fn open(
        &self,
        req: &ChatRequest,
        upstream_model: &str,
    ) -> Result<reqwest::Response, ProtocolError> {
        let body = to_responses_request(req, upstream_model, self.policy)
            .map_err(ProtocolError::UnsupportedRequest)?;
        let http = self.http.as_ref().ok_or_else(|| {
            ProtocolError::connect(std::io::Error::other(
                "Responses upstream requires a no-redirect HTTP client; client construction failed",
            ))
        })?;
        let bearer = self.bearer.bearer().await?;
        let mut builder = http
            .post(format!("{}/responses", self.base_url))
            .json(&body);
        if !bearer.is_empty() {
            builder = builder.bearer_auth(bearer.expose());
        }
        let response = builder
            .send()
            .await
            .map_err(ProtocolError::upstream_transport)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.bearer.rejected(&bearer).await;
        }
        super::openai::check_status(response).await
    }
}

#[async_trait]
impl Upstream for ResponsesUpstream {
    async fn send(
        &self,
        req: ChatRequest,
        upstream_model: &str,
    ) -> Result<ChatResponse, ProtocolError> {
        let requested = req.model.clone();
        let response = self.open(&req, upstream_model).await?;
        // A non-streaming completion always reports usage.
        sse::collect(
            sse::chat_chunks(response, requested.clone(), true),
            requested,
        )
        .await
    }

    async fn stream(
        &self,
        req: ChatRequest,
        upstream_model: &str,
    ) -> Result<StreamedChunks, ProtocolError> {
        let requested = req.model.clone();
        let include_usage = req
            .rest
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let response = self.open(&req, upstream_model).await?;
        Ok(StreamedChunks {
            // The plan route sends no Content-Type; the relay's
            // `text/event-stream` default is the right answer for both.
            content_type: None,
            cache_control: None,
            chunks: sse::chat_chunks(response, requested, include_usage),
        })
    }
}
