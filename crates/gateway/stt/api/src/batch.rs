//! OpenAI-compatible multipart transcription handling.

use std::io::Cursor;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use gateway_protocol::wire::ErrorEnvelope;
use gateway_stt_engine::{DecodeRequest, EnginePolicy};
use serde::Serialize;
use utoipa::ToSchema;

use crate::artifacts::SpeechError;
use crate::generation::GenerationState;
use crate::guidance::prompt_terms;

/// The batch transcription route's path.
pub const TRANSCRIPTIONS_PATH: &str = "/v1/audio/transcriptions";

const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const BODY_LIMIT: usize = MAX_AUDIO_BYTES + 1024 * 1024;
/// The whisper workers decode with this language fixed.
const LANGUAGE: &str = "en";

/// The reply dialect a request asks for in `response_format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
#[schema(as = TranscriptionResponseFormat, rename_all = "snake_case")]
enum ResponseFormat {
    Json,
    VerboseJson,
}

#[derive(Debug)]
struct TranscriptionForm {
    file: Vec<u8>,
    model: String,
    prompt: String,
    format: ResponseFormat,
    granularities: Vec<TimestampGranularity>,
}

/// One value of `timestamp_granularities[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
#[schema(rename_all = "lowercase")]
enum TimestampGranularity {
    Word,
    Segment,
}

fn default_granularities() -> Vec<TimestampGranularity> {
    vec![TimestampGranularity::Segment]
}

/// A basic OpenAI transcription response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
struct JsonTranscription {
    /// The decoded transcript.
    pub text: String,
}

/// One clip-level segment in a verbose transcription response.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
struct TranscriptionSegment {
    /// Zero-based segment identifier.
    pub id: u32,
    /// Segment start in seconds.
    pub start: f64,
    /// Segment end in seconds.
    pub end: f64,
    /// Text decoded for the segment.
    pub text: String,
}

/// An OpenAI verbose transcription response.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
struct VerboseJsonTranscription {
    /// Requested task name.
    pub task: &'static str,
    /// The transcription language, always `en`.
    pub language: &'static str,
    /// Audio duration in seconds.
    pub duration: f64,
    /// The decoded transcript.
    pub text: String,
    /// Clip-level segments when segment granularity was requested.
    pub segments: Vec<TranscriptionSegment>,
    /// Word timestamps. The speech engine produces segments only, so
    /// this array stays empty when word granularity is requested.
    pub words: Vec<serde_json::Value>,
}

/// A successful transcription in the requested OpenAI JSON dialect.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(untagged)]
enum TranscriptionResponse {
    /// The compact `json` response.
    Json(JsonTranscription),
    /// The `verbose_json` response.
    VerboseJson(VerboseJsonTranscription),
}

/// The form fields the route reads. A part with another name is ignored.
// The handler walks the parts by hand; this type names the form for the
// OpenAPI description only.
#[derive(ToSchema)]
#[expect(
    dead_code,
    reason = "the OpenAPI description reads this type; the handler parses the parts itself"
)]
struct TranscriptionUpload {
    /// The audio to transcribe, as a WAV file of 16 kHz mono samples. A
    /// file over 25 MiB is refused with 413 while it is read, and any other
    /// format, sample rate, or channel count is refused with 400.
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
    /// A speech model name that `GET /v1/models` lists: the configured
    /// interim or final model, or `realtime-transcribe`, which decodes with
    /// the final model and exists only while one is configured.
    model: String,
    /// Accepted only as `en`, the one language the speech models
    /// transcribe. Any other value is refused with 400.
    #[schema(required = false, pattern = "^en$")]
    language: String,
    /// `json`, the default, or `verbose_json`. Any other value, including
    /// OpenAI's `text`, `srt`, and `vtt`, is refused with 400.
    #[schema(required = false)]
    response_format: ResponseFormat,
    /// Repeatable. A valid value changes nothing: a `verbose_json` reply
    /// always holds one segment for the whole clip and no word timestamps.
    #[schema(rename = "timestamp_granularities[]", required = false)]
    timestamp_granularities: Vec<TimestampGranularity>,
    /// Validated, then not used: a finite number of zero or more, or the
    /// request is refused with 400.
    #[schema(required = false, minimum = 0)]
    temperature: f32,
    /// Comma-separated terms the decoder is biased toward, read as a term
    /// list rather than OpenAI's free-text prompt. Terms that do not fit the
    /// decoder's prompt are dropped from the end.
    #[schema(required = false)]
    prompt: String,
}

pub(crate) fn routes(state: GenerationState) -> Router {
    Router::new()
        .route(TRANSCRIPTIONS_PATH, post(handler))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Transcribes one uploaded WAV clip with a loaded speech model, in the
/// shape of OpenAI's transcription route.
///
/// The reply is `{"text"}` by default, or the `verbose_json` object when
/// `response_format` asks for it.
#[utoipa::path(
    post,
    path = TRANSCRIPTIONS_PATH,
    operation_id = "post_v1_audio_transcriptions",
    tag = "stt",
    request_body(content = inline(TranscriptionUpload), content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "The transcript, in the dialect `response_format` named.", body = TranscriptionResponse),
        (status = 400, description = "The body is not readable multipart form data, lacks `file` or `model`, holds a refused field value, or carries audio that is not a 16 kHz mono WAV.", body = ErrorEnvelope),
        (status = 401, description = "The Gateway's auth layer refused the caller: the bearer key was missing or did not match `server.key`.", body = ErrorEnvelope),
        (status = 403, description = "No body. The host wall refused a request whose authority is not the bound loopback socket."),
        (status = 404, description = "`model` names no speech model the gateway serves right now, which is every name while the speech models are not loaded.", body = ErrorEnvelope),
        (status = 413, description = "The `file` field passed 25 MiB.", body = ErrorEnvelope),
        (status = 500, description = "Decoding failed after every check passed, which includes a speech worker whose queue is full. The `message` is always `transcription failed`.", body = ErrorEnvelope),
    ),
    extensions(
        ("x-runvil-standard" = json!("openai")),
        ("x-runvil-feature" = json!("stt")),
        ("x-runvil-loopback-trust" = json!(true)),
    ),
)]
async fn handler(
    State(state): State<GenerationState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    let multipart = match multipart {
        Ok(multipart) => multipart,
        Err(error) => {
            return openai_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "malformed_request",
                &format!("malformed request: {error}"),
            );
        }
    };
    match transcribe(&state, multipart).await {
        Ok(response) => response,
        Err(error) => error_response(&error),
    }
}

async fn transcribe(
    state: &GenerationState,
    multipart: Multipart,
) -> Result<Response, SpeechError> {
    let form = parse_form(multipart).await?;
    let Some((generation, mode)) = state.select(&form.model) else {
        return Err(SpeechError::ModelNotFound(form.model));
    };
    let (samples, duration) = decode_wav(&form.file)?;
    let mut guidance = generation.guidance().to_vec();
    guidance.extend(prompt_terms(&form.prompt));
    let text = generation
        .decode(DecodeRequest::new(mode, samples, guidance, String::new()))
        .await
        .map_err(SpeechError::Inference)?
        .into_text();
    Ok(axum::Json(response(&form, text, duration)).into_response())
}

async fn parse_form(mut multipart: Multipart) -> Result<TranscriptionForm, SpeechError> {
    let mut file = None;
    let mut model = None;
    let mut prompt = String::new();
    let mut format = ResponseFormat::Json;
    let mut granularities = default_granularities();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(SpeechError::Multipart)?
    {
        let Some(name) = field.name().map(str::to_owned) else {
            continue;
        };
        match name.as_str() {
            "file" => {
                let mut bytes = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(SpeechError::Multipart)? {
                    if bytes.len().saturating_add(chunk.len()) > MAX_AUDIO_BYTES {
                        return Err(SpeechError::FileTooLarge);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                file = Some(bytes);
            }
            "model" => model = Some(field_text(field).await?),
            "language" => {
                let value = field_text(field).await?;
                if value != LANGUAGE {
                    return Err(SpeechError::UnsupportedLanguage(value));
                }
            }
            "response_format" => {
                format = match field_text(field).await?.as_str() {
                    "json" => ResponseFormat::Json,
                    "verbose_json" => ResponseFormat::VerboseJson,
                    value => {
                        return Err(SpeechError::UnsupportedResponseFormat(value.to_owned()));
                    }
                };
            }
            "timestamp_granularities[]" => {
                granularities.push(match field_text(field).await?.as_str() {
                    "word" => TimestampGranularity::Word,
                    "segment" => TimestampGranularity::Segment,
                    value => {
                        return Err(SpeechError::InvalidField {
                            field: "timestamp_granularities[]",
                            value: value.to_owned(),
                        });
                    }
                });
            }
            "temperature" => {
                let value = field_text(field).await?;
                let parsed = value
                    .parse::<f32>()
                    .map_err(|_| SpeechError::InvalidField {
                        field: "temperature",
                        value: value.clone(),
                    })?;
                if !parsed.is_finite() || parsed < 0.0 {
                    return Err(SpeechError::InvalidField {
                        field: "temperature",
                        value,
                    });
                }
            }
            // The prompt's comma-separated terms join the configured
            // vocabulary as decode guidance.
            "prompt" => prompt = field_text(field).await?,
            _ => {}
        }
    }
    Ok(TranscriptionForm {
        file: file.ok_or(SpeechError::MissingField("file"))?,
        model: model.ok_or(SpeechError::MissingField("model"))?,
        prompt,
        format,
        granularities,
    })
}

async fn field_text(field: axum::extract::multipart::Field<'_>) -> Result<String, SpeechError> {
    field.text().await.map_err(SpeechError::Multipart)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "PCM normalization and clip duration intentionally convert bounded audio counts to floating point"
)]
fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, f64), SpeechError> {
    const SAMPLE_RATE_U32: u32 = 16_000;
    let mut reader =
        hound::WavReader::new(Cursor::new(bytes)).map_err(SpeechError::InvalidAudio)?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != SAMPLE_RATE_U32 {
        return Err(SpeechError::UnsupportedAudio {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
        });
    }
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(SpeechError::InvalidAudio)?,
        hound::SampleFormat::Int => {
            let denominator = 2_f32.powi(i32::from(spec.bits_per_sample.saturating_sub(1)));
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| value as f32 / denominator)
                        .map_err(SpeechError::InvalidAudio)
                })
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    let duration = samples.len() as f64 / EnginePolicy::SAMPLE_RATE as f64;
    Ok((samples, duration))
}

fn response(form: &TranscriptionForm, text: String, duration: f64) -> TranscriptionResponse {
    match form.format {
        ResponseFormat::Json => TranscriptionResponse::Json(JsonTranscription { text }),
        ResponseFormat::VerboseJson => {
            let segments = if form.granularities.contains(&TimestampGranularity::Segment) {
                vec![TranscriptionSegment {
                    id: 0,
                    start: 0.0,
                    end: duration,
                    text: text.clone(),
                }]
            } else {
                Vec::new()
            };
            TranscriptionResponse::VerboseJson(VerboseJsonTranscription {
                task: "transcribe",
                language: LANGUAGE,
                duration,
                text,
                segments,
                words: Vec::new(),
            })
        }
    }
}

fn error_response(error: &SpeechError) -> Response {
    let (status, kind, code) = if error.model_not_found().is_some() {
        (
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "model_not_found",
        )
    } else if error.is_file_too_large() {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
            "file_too_large",
        )
    } else if error.is_inference() {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "transcription_error",
        )
    } else {
        (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "malformed_request",
        )
    };
    let message = if error.is_inference() {
        "transcription failed".to_owned()
    } else if error.model_not_found().is_some() || error.is_file_too_large() {
        error.to_string()
    } else {
        format!("malformed request: {error}")
    };
    openai_error_response(status, kind, code, &message)
}

fn openai_error_response(
    status: StatusCode,
    kind: &'static str,
    code: &'static str,
    message: &str,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": kind,
                "code": code,
            }
        })),
    )
        .into_response()
}

#[cfg(all(test, not(miri)))]
#[path = "batch-native-tests.rs"]
mod native_tests;

#[cfg(test)]
#[path = "batch-tests.rs"]
mod tests;
