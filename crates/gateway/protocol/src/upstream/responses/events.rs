//! Responses stream events to chat completion chunks.
//!
//! [`Translator`] reads one decoded Responses event at a time and says what
//! it means for the chat stream: chunks to relay, the final chunks, or a
//! failure. Text and tool-call arguments are relayed as their deltas arrive.
//! Argument deltas that arrive before their call's `output_item.added` are
//! held and sent with the call's start. Each `output_item.done` is checked
//! against what was streamed: missing argument text is sent then, so a
//! backend that skips argument deltas still delivers whole tool calls, and
//! arguments that contradict what was already sent fail the response rather
//! than relay a corrupted call. A call that ends with no arguments at all
//! gets `{}`, so its arguments always parse as JSON.
//!
//! `response.completed` and `response.incomplete` end the stream as a
//! success, the second with `finish_reason` `length` or `content_filter`.
//! Usage follows the chat contract: only when the caller asked for it
//! (`stream_options.include_usage`), as `null` on every other chunk and the
//! real figures in a separate last chunk with empty `choices`, and never
//! invented when the backend reports none. Reasoning
//! and other item kinds have no chat equivalent and are not relayed.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::error::ProtocolError;
use crate::upstream::openai::{MAX_ERROR_MESSAGE_CHARS, escape_control};
use crate::wire::{ChatChunk, ChatChunkChoice};

/// What one Responses event means for the chat stream.
#[derive(Debug)]
pub(super) enum Step {
    /// Chunks to relay (possibly none); the stream continues.
    Continue(Vec<ChatChunk>),
    /// The last chunks; the response ended normally.
    Finish(Vec<ChatChunk>),
    /// The response failed; nothing more follows.
    Fail(ProtocolError),
}

/// One function call seen in the stream, in first-seen order: its position
/// is its chat `tool_calls` index.
#[derive(Debug)]
struct ToolCall {
    item_id: Option<String>,
    output_index: Option<u64>,
    arguments_sent: String,
}

/// Argument text that arrived before its call's metadata.
#[derive(Debug)]
struct EarlyArguments {
    item_id: Option<String>,
    output_index: Option<u64>,
    text: String,
}

/// Whether `item_id` or `output_index` names the item recorded with
/// `recorded_id` and `recorded_index`.
fn same_item(
    recorded_id: Option<&str>,
    recorded_index: Option<u64>,
    item_id: Option<&str>,
    output_index: Option<u64>,
) -> bool {
    (item_id.is_some() && recorded_id == item_id)
        || (output_index.is_some() && recorded_index == output_index)
}

/// The per-response translation state.
#[derive(Debug)]
pub(super) struct Translator {
    requested: String,
    id: String,
    created: u64,
    role_sent: bool,
    include_usage: bool,
    calls: Vec<ToolCall>,
    early: Vec<EarlyArguments>,
}

impl Translator {
    /// A translator whose chunks name `requested`, the caller's model, and
    /// that adds the usage chunk only when `include_usage` is set.
    pub(super) fn new(requested: String, include_usage: bool) -> Translator {
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Translator {
            requested,
            id: "chatcmpl-responses".to_owned(),
            created,
            role_sent: false,
            include_usage,
            calls: Vec::new(),
            early: Vec::new(),
        }
    }

    /// Translates one decoded event.
    pub(super) fn step(&mut self, event: &Value) -> Step {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.created" | "response.in_progress" => {
                self.note_response(event.get("response"));
                Step::Continue(Vec::new())
            }
            "response.output_text.delta" => Step::Continue(self.text_delta(event, "content")),
            "response.refusal.delta" => Step::Continue(self.text_delta(event, "refusal")),
            "response.output_item.added" => self.item_added(event),
            "response.function_call_arguments.delta" => Step::Continue(self.arguments_delta(event)),
            "response.output_item.done" => self.item_done(event),
            "response.completed" => Step::Finish(self.finish(event, None)),
            "response.incomplete" => {
                let reason = event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str);
                let finish = if reason == Some("content_filter") {
                    "content_filter"
                } else {
                    "length"
                };
                Step::Finish(self.finish(event, Some(finish)))
            }
            "response.failed" => Step::Fail(failure(event.pointer("/response/error"))),
            // The documented event carries the details as its own fields; the
            // ChatGPT plan route nests them under `error`.
            "error" => Step::Fail(failure(
                event
                    .get("error")
                    .filter(|error| error.is_object())
                    .or(Some(event)),
            )),
            _ => Step::Continue(Vec::new()),
        }
    }

    /// Takes the response id and creation time when the event carries them.
    fn note_response(&mut self, response: Option<&Value>) {
        let Some(response) = response else { return };
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            id.clone_into(&mut self.id);
        }
        if let Some(created) = response.get("created_at").and_then(Value::as_u64) {
            self.created = created;
        }
    }

    fn text_delta(&mut self, event: &Value, field: &str) -> Vec<ChatChunk> {
        let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return Vec::new();
        }
        let mut delta = Map::new();
        delta.insert(field.to_owned(), Value::String(text.to_owned()));
        vec![self.chunk(delta, None)]
    }

    fn item_added(&mut self, event: &Value) -> Step {
        let Some(item) = function_call(event) else {
            return Step::Continue(Vec::new());
        };
        let output_index = event.get("output_index").and_then(Value::as_u64);
        if self
            .locate(item.get("id").and_then(Value::as_str), output_index)
            .is_some()
        {
            return Step::Continue(Vec::new());
        }
        match self.start_call(item, output_index, false) {
            Ok(chunks) => Step::Continue(chunks),
            Err(error) => Step::Fail(error),
        }
    }

    fn arguments_delta(&mut self, event: &Value) -> Vec<ChatChunk> {
        let item_id = event.get("item_id").and_then(Value::as_str);
        let output_index = event.get("output_index").and_then(Value::as_u64);
        let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return Vec::new();
        }
        let Some(index) = self.locate(item_id, output_index) else {
            // No `output_item.added` yet: hold the text for the call's start.
            match self.early.iter_mut().find(|early| {
                same_item(
                    early.item_id.as_deref(),
                    early.output_index,
                    item_id,
                    output_index,
                )
            }) {
                Some(early) => early.text.push_str(text),
                None => self.early.push(EarlyArguments {
                    item_id: item_id.map(str::to_owned),
                    output_index,
                    text: text.to_owned(),
                }),
            }
            return Vec::new();
        };
        self.calls[index].arguments_sent.push_str(text);
        vec![self.chunk(arguments_only(index, text), None)]
    }

    fn item_done(&mut self, event: &Value) -> Step {
        let Some(item) = function_call(event) else {
            return Step::Continue(Vec::new());
        };
        let output_index = event.get("output_index").and_then(Value::as_u64);
        let Some(index) = self.locate(item.get("id").and_then(Value::as_str), output_index) else {
            return match self.start_call(item, output_index, true) {
                Ok(mut chunks) => {
                    chunks.extend(self.close_empty(self.calls.len() - 1));
                    Step::Continue(chunks)
                }
                Err(error) => Step::Fail(error),
            };
        };
        let full = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        let sent = self.calls[index].arguments_sent.clone();
        if !full.starts_with(sent.as_str()) {
            return Step::Fail(contradiction(index));
        }
        let mut chunks = Vec::new();
        if full.len() > sent.len() {
            let missing = &full[sent.len()..];
            self.calls[index].arguments_sent.push_str(missing);
            chunks.push(self.chunk(arguments_only(index, missing), None));
        }
        chunks.extend(self.close_empty(index));
        Step::Continue(chunks)
    }

    /// Sends `{}` for a call that ended with no argument text, so its
    /// arguments parse as JSON.
    fn close_empty(&mut self, index: usize) -> Option<ChatChunk> {
        if !self.calls[index].arguments_sent.is_empty() {
            return None;
        }
        self.calls[index].arguments_sent.push_str("{}");
        Some(self.chunk(arguments_only(index, "{}"), None))
    }

    /// Registers a new function call and sends its id, name, and the
    /// arguments known so far: the held early text reconciled with the
    /// item's own. A done item (`complete`) is authoritative, so early text
    /// must be its prefix. An added item holds a prefix at most, so one of
    /// the two must extend the other. Anything else fails the response.
    fn start_call(
        &mut self,
        item: &Value,
        output_index: Option<u64>,
        complete: bool,
    ) -> Result<Vec<ChatChunk>, ProtocolError> {
        let index = self.calls.len();
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .map_or_else(|| format!("call_{index}"), str::to_owned);
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let item_id = item.get("id").and_then(Value::as_str);
        let early = self
            .early
            .iter()
            .position(|early| {
                same_item(
                    early.item_id.as_deref(),
                    early.output_index,
                    item_id,
                    output_index,
                )
            })
            .map(|position| self.early.remove(position).text)
            .unwrap_or_default();
        let held = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        let arguments = if held.starts_with(early.as_str()) {
            held.to_owned()
        } else if !complete && early.starts_with(held) {
            early
        } else {
            return Err(contradiction(index));
        };
        self.calls.push(ToolCall {
            item_id: item_id.map(str::to_owned),
            output_index,
            arguments_sent: arguments.clone(),
        });
        let mut delta = Map::new();
        delta.insert(
            "tool_calls".to_owned(),
            json!([{
                "index": index,
                "id": call_id,
                "type": "function",
                "function": { "name": name, "arguments": arguments },
            }]),
        );
        Ok(vec![self.chunk(delta, None)])
    }

    /// The position of the call with this item id or output index.
    fn locate(&self, item_id: Option<&str>, output_index: Option<u64>) -> Option<usize> {
        self.calls.iter().position(|call| {
            same_item(
                call.item_id.as_deref(),
                call.output_index,
                item_id,
                output_index,
            )
        })
    }

    /// The final chunks: the finish reason, then the usage when the caller
    /// asked for it and the backend reported it.
    fn finish(&mut self, event: &Value, reason: Option<&str>) -> Vec<ChatChunk> {
        self.note_response(event.get("response"));
        let finish = reason.unwrap_or(if self.calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        });
        let mut chunks = vec![self.chunk(Map::new(), Some(finish))];
        let usage = event
            .pointer("/response/usage")
            .filter(|usage| usage.is_object());
        if self.include_usage
            && let Some(usage) = usage
        {
            let mut rest = self.envelope();
            rest.insert("usage".to_owned(), chat_usage(usage));
            chunks.push(ChatChunk {
                model: self.requested.clone(),
                choices: Vec::new(),
                rest,
            });
        }
        chunks
    }

    /// The `id`, `object` and `created` every chunk carries.
    fn envelope(&self) -> Map<String, Value> {
        let mut rest = Map::new();
        rest.insert("id".to_owned(), Value::String(self.id.clone()));
        rest.insert(
            "object".to_owned(),
            Value::String("chat.completion.chunk".to_owned()),
        );
        rest.insert("created".to_owned(), Value::from(self.created));
        rest
    }

    fn chunk(&mut self, mut delta: Map<String, Value>, finish: Option<&str>) -> ChatChunk {
        if !self.role_sent {
            delta.insert("role".to_owned(), Value::String("assistant".to_owned()));
            self.role_sent = true;
        }
        let mut choice_rest = Map::new();
        choice_rest.insert(
            "finish_reason".to_owned(),
            finish.map_or(Value::Null, |reason| Value::String(reason.to_owned())),
        );
        let mut rest = self.envelope();
        if self.include_usage {
            // The chat contract: every chunk but the last carries a null
            // usage when the caller asked for usage.
            rest.insert("usage".to_owned(), Value::Null);
        }
        ChatChunk {
            model: self.requested.clone(),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: Value::Object(delta),
                rest: choice_rest,
            }],
            rest,
        }
    }
}

/// The failure for arguments that contradict text already relayed or held.
fn contradiction(index: usize) -> ProtocolError {
    tracing::warn!(
        index,
        "function call arguments contradict the streamed deltas"
    );
    ProtocolError::upstream_protocol(std::io::Error::other(
        "function call arguments contradict the streamed deltas",
    ))
}

/// The event's item, when it is a function call.
fn function_call(event: &Value) -> Option<&Value> {
    event
        .get("item")
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
}

fn arguments_only(index: usize, arguments: &str) -> Map<String, Value> {
    let mut delta = Map::new();
    delta.insert(
        "tool_calls".to_owned(),
        json!([{ "index": index, "function": { "arguments": arguments } }]),
    );
    delta
}

/// Responses usage in the chat completions shape.
fn chat_usage(usage: &Value) -> Value {
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input + output);
    let mut chat = json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": total,
    });
    if let Some(cached) = usage.pointer("/input_tokens_details/cached_tokens") {
        chat["prompt_tokens_details"] = json!({ "cached_tokens": cached });
    }
    if let Some(reasoning) = usage.pointer("/output_tokens_details/reasoning_tokens") {
        chat["completion_tokens_details"] = json!({ "reasoning_tokens": reasoning });
    }
    chat
}

/// A failed response as a [`ProtocolError::UpstreamStatus`]. A usage or rate
/// limit maps to 429, an invalid request to 400, anything else to 502. Only
/// the error's bounded, control-escaped `code`, `type`, and `message` are
/// logged or kept.
fn failure(error: Option<&Value>) -> ProtocolError {
    let field = |name: &str| {
        error
            .and_then(|error| error.get(name))
            .and_then(Value::as_str)
            .map_or_else(String::new, |text| {
                escape_control(text, MAX_ERROR_MESSAGE_CHARS)
            })
    };
    let (code, kind, message) = (field("code"), field("type"), field("message"));
    tracing::info!(
        code = %code,
        r#type = %kind,
        error.message = %message,
        "upstream response failed"
    );
    let status = if ["limit", "quota", "rate"]
        .iter()
        .any(|word| code.contains(word))
    {
        429
    } else if kind == "invalid_request_error" {
        400
    } else {
        502
    };
    let body = json!({ "error": { "code": code, "type": kind, "message": message } });
    ProtocolError::upstream_status(status, body.to_string())
}
