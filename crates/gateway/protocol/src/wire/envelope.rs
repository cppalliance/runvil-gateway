//! The OpenAI error envelope's shape, named for the OpenAPI description.

use utoipa::ToSchema;

/// The body of every JSON error response, in the OpenAI shape:
/// `{"error": {"message", "type", "code"}}`.
// `ProtocolError::envelope`, the gateway's own error type, and the speech
// crate's batch route each write this shape as a JSON literal. This type
// names the shape for the OpenAPI description and is never built. Those
// literals write exactly these fields, so `deny_unknown_fields`, read here
// by utoipa alone, closes both schemas.
#[derive(Debug, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    /// The error itself.
    pub error: ErrorDetail,
}

/// The error: a message, its OpenAI error class, and its code.
#[derive(Debug, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetail {
    /// A human-readable message.
    pub message: String,
    /// The OpenAI error class, such as `invalid_request_error`,
    /// `authentication_error`, `rate_limit_error`, or `server_error`.
    #[schema(rename = "type")]
    pub kind: String,
    /// The machine-readable error code, such as `model_not_found`.
    pub code: String,
}
