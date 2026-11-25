//! The Responses event stream: SSE framing, translation to chat chunks, and
//! the fold that turns those chunks into one non-streaming response.
//!
//! Framing follows the SSE parsing rules: lines end in CRLF, LF or CR; one
//! leading byte-order mark is dropped; an event's `data:` lines are joined;
//! a blank line dispatches the event; and an event still unfinished when the
//! body ends is discarded, never dispatched. The `event:` field is ignored,
//! because every event names itself in its JSON `type`. The upstream
//! `Content-Type` is not checked, because the ChatGPT plan route sends none.
//! A stream that ends before `response.completed` (or
//! `response.incomplete`) ends with an error, so a truncated reply is never
//! relayed as a complete one.

use std::collections::VecDeque;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::{Map, Value, json};

use super::events::{Step, Translator};
use crate::error::ProtocolError;
use crate::wire::{ChatChunk, ChatResponse};

/// The largest single event accepted. An `output_item.done` repeats a whole
/// item, so this is generous; a larger event ends the stream with an error
/// rather than growing the buffer without bound.
const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// The decoder's state between polls.
struct Decoder {
    bytes: BoxStream<'static, reqwest::Result<Bytes>>,
    buffer: Vec<u8>,
    data: String,
    translator: Translator,
    ready: VecDeque<Result<ChatChunk, ProtocolError>>,
    /// Whether the leading byte-order mark check has run.
    bom_checked: bool,
    ended: bool,
}

impl Decoder {
    /// Decodes the event held in `data`, queueing what it produces.
    fn dispatch(&mut self) {
        let data = std::mem::take(&mut self.data);
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let event = match serde_json::from_str::<Value>(&data) {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, "skipping undecodable upstream event");
                return;
            }
        };
        match self.translator.step(&event) {
            Step::Continue(chunks) => self.ready.extend(chunks.into_iter().map(Ok)),
            Step::Finish(chunks) => {
                self.ready.extend(chunks.into_iter().map(Ok));
                self.ended = true;
            }
            Step::Fail(error) => {
                self.ready.push_back(Err(error));
                self.ended = true;
            }
        }
    }

    /// Takes one complete line from the buffer and applies it. A CR as the
    /// buffer's last byte waits for the next read, which may hold its LF.
    fn take_line(&mut self) -> bool {
        if !self.bom_checked {
            if self.buffer.len() < 3
                && !self.buffer.is_empty()
                && b"\xEF\xBB\xBF".starts_with(&self.buffer)
            {
                return false;
            }
            if self.buffer.starts_with(b"\xEF\xBB\xBF") {
                self.buffer.drain(..3);
            }
            self.bom_checked = !self.buffer.is_empty();
        }
        let Some(end) = self
            .buffer
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
        else {
            return false;
        };
        let width = match (self.buffer[end], self.buffer.get(end + 1)) {
            (b'\r', Some(b'\n')) => 2,
            (b'\r', None) => return false,
            _ => 1,
        };
        let raw: Vec<u8> = self.buffer.drain(..end + width).collect();
        let line = String::from_utf8_lossy(&raw[..end]);
        let line = line.as_ref();
        if line.is_empty() {
            self.dispatch();
        } else if let Some(data) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
        }
        true
    }

    /// Fails the stream: the error is the last item.
    fn fail(&mut self, error: ProtocolError) {
        self.ready.push_back(Err(error));
        self.ended = true;
    }
}

/// Translates a Responses SSE body into chat completion chunks naming
/// `requested`. Dropping the stream drops the upstream response, which
/// aborts the connection.
pub(super) fn chat_chunks(
    response: reqwest::Response,
    requested: String,
    include_usage: bool,
) -> BoxStream<'static, Result<ChatChunk, ProtocolError>> {
    let decoder = Decoder {
        bytes: response.bytes_stream().boxed(),
        buffer: Vec::new(),
        data: String::new(),
        translator: Translator::new(requested, include_usage),
        ready: VecDeque::new(),
        bom_checked: false,
        ended: false,
    };
    futures_util::stream::unfold(decoder, |mut decoder| async move {
        loop {
            if let Some(item) = decoder.ready.pop_front() {
                return Some((item, decoder));
            }
            if decoder.ended {
                return None;
            }
            if decoder.take_line() {
                continue;
            }
            if decoder.buffer.len() + decoder.data.len() > MAX_EVENT_BYTES {
                decoder.fail(ProtocolError::upstream_protocol(std::io::Error::other(
                    "upstream event exceeds the size limit",
                )));
                continue;
            }
            match decoder.bytes.next().await {
                Some(Ok(bytes)) => decoder.buffer.extend_from_slice(&bytes),
                Some(Err(error)) => decoder.fail(ProtocolError::upstream_transport(error)),
                None => {
                    // A lone CR at the very end still ends its line.
                    if decoder.buffer.last() == Some(&b'\r') {
                        decoder.buffer.push(b'\n');
                        while !decoder.ended && decoder.take_line() {}
                    }
                    // Whatever is left is an unfinished event: discarded.
                    decoder.buffer.clear();
                    decoder.data.clear();
                    if !decoder.ended {
                        decoder.fail(ProtocolError::upstream_protocol(std::io::Error::other(
                            "upstream stream ended before the response completed",
                        )));
                    }
                }
            }
        }
    })
    .boxed()
}

/// Folds a translated chunk stream into one chat completion: the text
/// concatenated, tool calls assembled by index, and the last finish reason
/// and usage kept. The first error ends the fold.
pub(super) async fn collect(
    mut chunks: BoxStream<'static, Result<ChatChunk, ProtocolError>>,
    requested: String,
) -> Result<ChatResponse, ProtocolError> {
    let mut content = String::new();
    let mut refusal = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut finish = Value::Null;
    let mut rest = Map::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        for key in ["id", "created", "usage"] {
            if let Some(value) = chunk.rest.get(key).filter(|value| !value.is_null()) {
                rest.insert(key.to_owned(), value.clone());
            }
        }
        for choice in &chunk.choices {
            if let Some(reason) = choice.rest.get("finish_reason").filter(|v| !v.is_null()) {
                finish = reason.clone();
            }
            let delta = &choice.delta;
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                content.push_str(text);
            }
            if let Some(text) = delta.get("refusal").and_then(Value::as_str) {
                refusal.push_str(text);
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                merge_call(&mut calls, call);
            }
        }
    }
    rest.insert(
        "object".to_owned(),
        Value::String("chat.completion".to_owned()),
    );
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !calls.is_empty() { Value::Null } else { Value::String(content) },
    });
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    if !refusal.is_empty() {
        message["refusal"] = Value::String(refusal);
    }
    Ok(ChatResponse {
        model: requested,
        choices: vec![json!({ "index": 0, "message": message, "finish_reason": finish })],
        rest,
    })
}

/// Applies one `tool_calls` delta to the assembled calls.
fn merge_call(calls: &mut Vec<Value>, delta: &Value) {
    let Some(index) = delta
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
    else {
        return;
    };
    while calls.len() <= index {
        calls.push(
            json!({ "id": "", "type": "function", "function": { "name": "", "arguments": "" } }),
        );
    }
    let call = &mut calls[index];
    if let Some(id) = delta.get("id").and_then(Value::as_str) {
        call["id"] = Value::String(id.to_owned());
    }
    if let Some(name) = delta.pointer("/function/name").and_then(Value::as_str)
        && !name.is_empty()
    {
        call["function"]["name"] = Value::String(name.to_owned());
    }
    if let Some(arguments) = delta.pointer("/function/arguments").and_then(Value::as_str) {
        let mut joined = call["function"]["arguments"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        joined.push_str(arguments);
        call["function"]["arguments"] = Value::String(joined);
    }
}
