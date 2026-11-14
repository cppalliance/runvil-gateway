//! Fetching and projecting the signed-in account's ChatGPT plan model catalog
//! from `{api_base}/models`.
//!
//! Only `visibility == "list"` entries are kept, projected onto the closed
//! triple `(slug, display_name, context_window)`. Redirects are never
//! followed, bodies are capped at [`MAX_MODELS_BODY`], and upstream headers,
//! extra JSON fields, hidden slugs, and error bodies never enter errors or
//! logs.

use std::io;
use std::time::Duration;

use gateway_chatgpt_plan::plan::{ChatGptPlan, SessionFence};
use gateway_protocol::ProtocolError;
use gateway_protocol::http_util::read_bytes_capped;
use reqwest::StatusCode;
use serde::Deserialize;

use crate::error::GatewayError;

/// Maximum bytes accepted from `{api_base}/models` (1 MiB).
const MAX_MODELS_BODY: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Closed projection of one user-visible model from OpenAI's `/models` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanModelEntry {
    /// Upstream model identifier (`slug`).
    pub(crate) slug: String,
    /// Human-readable label when supplied and non-empty.
    pub(crate) display_name: Option<String>,
    /// Context window in tokens when supplied.
    pub(crate) context_window: Option<u32>,
}

#[derive(Deserialize)]
struct ModelsEnvelope {
    models: Vec<RawPlanModel>,
}

#[derive(Deserialize)]
struct RawPlanModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    context_window: Option<u32>,
}

/// Builds the dedicated HTTP client for `{api_base}/models`: bounded timeouts
/// and `Policy::none()` so the plan bearer never follows a redirect. Returns
/// `None` if construction fails instead of falling back to a default client
/// whose redirect policy would follow redirects.
pub(super) fn models_http_client() -> Option<reqwest::Client> {
    let builder = || {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
    };
    builder().build().or_else(|_| builder().build()).ok()
}

/// Snapshots a [`SessionFence`] on `plan`, fetches `{api_base}/models` with
/// the plan bearer (retrying once on `401` after evicting the cached token),
/// and returns the fence plus the projected `visibility == "list"` models.
pub(super) async fn fetch(
    client: &reqwest::Client,
    api_base: &str,
    plan: &ChatGptPlan,
) -> Result<(SessionFence, Vec<PlanModelEntry>), GatewayError> {
    let fence = plan.current_fence()?;
    let url = format!("{}/models", api_base.trim_end_matches('/'));

    let bytes = fetch_bytes(client, &url, plan).await?;
    let envelope: ModelsEnvelope = serde_json::from_slice(&bytes)
        .map_err(|_| protocol_decode_error("invalid /models JSON payload"))?;

    let mut projected = Vec::new();
    for item in envelope.models {
        if item.visibility.as_deref() != Some("list") {
            continue;
        }
        let Some(slug) = clean_slug(&item.slug) else {
            continue;
        };
        projected.push(PlanModelEntry {
            slug,
            display_name: item.display_name.as_deref().and_then(clean_display_name),
            context_window: item.context_window,
        });
    }
    Ok((fence, projected))
}

async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    plan: &ChatGptPlan,
) -> Result<Vec<u8>, GatewayError> {
    for attempt in 0..2 {
        let token = plan.access_token().await?;
        let response = client
            .get(url)
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|error| {
                if error.is_connect() {
                    GatewayError::Protocol(ProtocolError::connect(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "could not connect to ChatGPT models endpoint",
                    )))
                } else {
                    GatewayError::Protocol(ProtocolError::transport(io::Error::other(
                        "ChatGPT models request failed",
                    )))
                }
            })?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED && attempt == 0 {
            plan.reject_access(&token);
            continue;
        }
        if !status.is_success() {
            return Err(ProtocolError::UpstreamStatus {
                status: status.as_u16(),
                body: String::new(),
            }
            .into());
        }
        if response
            .content_length()
            .is_some_and(|announced| announced > MAX_MODELS_BODY as u64)
        {
            return Err(protocol_decode_error("/models body exceeds size cap"));
        }
        let bytes = read_bytes_capped(response, MAX_MODELS_BODY + 1)
            .await
            .map_err(|_| {
                GatewayError::Protocol(ProtocolError::transport(io::Error::other(
                    "failed reading ChatGPT models response",
                )))
            })?;
        if bytes.len() > MAX_MODELS_BODY {
            return Err(protocol_decode_error("/models body exceeds size cap"));
        }
        return Ok(bytes);
    }
    Err(ProtocolError::UpstreamStatus {
        status: StatusCode::UNAUTHORIZED.as_u16(),
        body: String::new(),
    }
    .into())
}

fn protocol_decode_error(reason: &'static str) -> GatewayError {
    ProtocolError::UpstreamProtocol(Box::new(io::Error::new(io::ErrorKind::InvalidData, reason)))
        .into()
}

fn clean_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let ok = !trimmed.is_empty()
        && trimmed.len() <= 128
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c));
    ok.then(|| trimmed.to_owned())
}

fn clean_display_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}
