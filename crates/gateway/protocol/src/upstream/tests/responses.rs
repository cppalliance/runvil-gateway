//! Tests for the Responses upstream: the request it sends, the event stream
//! translated to chat chunks, the non-streaming fold, and its failures.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::wire::RequestPolicy;

mod failures;

/// A mock Responses backend: answers one request with `status_line` and
/// `body`, written in `pieces` separate writes and with **no**
/// `Content-Type` (the ChatGPT plan route sends none). Returns the base URL
/// and the captured raw request.
fn serve_events(status_line: &str, body: &str, pieces: usize) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock backend");
    let addr = listener.local_addr().expect("addr");
    let head = format!("HTTP/1.1 {status_line}\r\nConnection: close\r\n\r\n");
    let body = body.as_bytes().to_vec();
    let handle = thread::spawn(move || -> String {
        let (mut stream, _) = listener.accept().expect("accept");
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let _ = stream.write_all(head.as_bytes());
        let size = body.len().div_ceil(pieces.max(1)).max(1);
        for piece in body.chunks(size) {
            let _ = stream.write_all(piece);
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(2));
        }
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{addr}"), handle)
}

fn sse(events: &[Value]) -> String {
    use std::fmt::Write as _;

    let mut body = String::new();
    for event in events {
        let kind = event["type"].as_str().unwrap_or("message");
        let _ = write!(body, "event: {kind}\ndata: {event}\n\n");
    }
    body
}

fn completed(usage: &Value) -> Value {
    json!({ "type": "response.completed",
            "response": { "id": "resp_1", "created_at": 1_763_000_000_u64, "status": "completed",
                          "output": [], "usage": usage } })
}

fn upstream(base: &str) -> ResponsesUpstream {
    ResponsesUpstream::new(
        base,
        Arc::new(StaticBearer::new(Secret::new("sk-test".to_owned()))),
        RequestPolicy::Plan,
    )
}

async fn all_chunks(upstream: &ResponsesUpstream) -> Vec<Result<ChatChunk, ProtocolError>> {
    all_chunks_of(upstream, request("caller-model")).await
}

/// A streaming request that asks for usage, as the harness does.
fn usage_request() -> ChatRequest {
    let mut req = request("caller-model");
    req.rest.insert(
        "stream_options".to_owned(),
        json!({ "include_usage": true }),
    );
    req
}

async fn all_chunks_of(
    upstream: &ResponsesUpstream,
    req: ChatRequest,
) -> Vec<Result<ChatChunk, ProtocolError>> {
    let mut streamed = upstream
        .stream(req, "backend-model")
        .await
        .expect("stream opens");
    let mut chunks = Vec::new();
    while let Some(item) = streamed.chunks.next().await {
        chunks.push(item);
    }
    chunks
}

#[tokio::test]
async fn text_streams_as_chat_chunks_and_unasked_usage_is_omitted() {
    // The plan route's quirk: response.completed carries an empty output, so
    // the text must come from the deltas. One "é" is split across writes.
    let body = sse(&[
        json!({ "type": "response.created", "response": { "id": "resp_1", "created_at": 1_763_000_000_u64 } }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "delta": "Caf" }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "delta": "é!" }),
        completed(
            &json!({ "input_tokens": 10, "output_tokens": 3, "total_tokens": 13,
                          "input_tokens_details": { "cached_tokens": 4 },
                          "output_tokens_details": { "reasoning_tokens": 1 } }),
        ),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 37);
    let chunks: Vec<ChatChunk> = all_chunks(&upstream(&base))
        .await
        .into_iter()
        .map(|item| item.expect("chunk ok"))
        .collect();
    let raw = handle.join().expect("server");

    assert!(raw.starts_with("POST /responses "), "request line: {raw}");
    assert!(
        raw.to_ascii_lowercase()
            .contains("authorization: bearer sk-test")
    );
    let sent: Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(sent["model"], "backend-model");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["store"], false);

    let text: String = chunks
        .iter()
        .filter_map(|chunk| {
            chunk.choices[0]
                .delta
                .get("content")
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(text, "Café!");
    assert!(chunks.iter().all(|chunk| chunk.model == "caller-model"));
    assert_eq!(chunks[0].choices[0].delta["role"], "assistant");
    assert_eq!(chunks[0].rest["id"], "resp_1");
    // Usage was not asked for, so no usage chunk follows the finish.
    let last = chunks.last().expect("a final chunk");
    assert_eq!(last.choices[0].rest["finish_reason"], "stop");
    assert!(chunks.iter().all(|chunk| !chunk.rest.contains_key("usage")));
}

#[tokio::test]
async fn asked_for_usage_arrives_as_a_last_chunk_with_empty_choices() {
    let body = sse(&[
        json!({ "type": "response.output_text.delta", "delta": "ok" }),
        completed(
            &json!({ "input_tokens": 10, "output_tokens": 3, "total_tokens": 13,
                          "input_tokens_details": { "cached_tokens": 4 },
                          "output_tokens_details": { "reasoning_tokens": 1 } }),
        ),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let chunks: Vec<ChatChunk> = all_chunks_of(&upstream(&base), usage_request())
        .await
        .into_iter()
        .map(|item| item.expect("chunk ok"))
        .collect();
    handle.join().expect("server");
    let finish = &chunks[chunks.len() - 2];
    assert_eq!(finish.choices[0].rest["finish_reason"], "stop");
    assert_eq!(
        finish.rest["usage"],
        Value::Null,
        "non-final chunks carry a null usage"
    );
    let last = chunks.last().expect("a usage chunk");
    assert!(last.choices.is_empty());
    assert_eq!(
        last.rest["usage"],
        json!({ "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13,
                "prompt_tokens_details": { "cached_tokens": 4 },
                "completion_tokens_details": { "reasoning_tokens": 1 } })
    );
}

#[tokio::test]
async fn a_streamed_tool_call_folds_into_one_non_streaming_message() {
    let body = sse(&[
        json!({ "type": "response.output_item.added", "output_index": 1,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9",
                          "name": "weather", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 1, "delta": "{\"city\":" }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 1, "delta": "\"Siem Reap\"}" }),
        json!({ "type": "response.output_item.done", "output_index": 1,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "call_9",
                          "name": "weather", "arguments": "{\"city\":\"Siem Reap\"}" } }),
        completed(&json!({ "input_tokens": 5, "output_tokens": 7 })),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let response = upstream(&base)
        .send(request("caller-model"), "backend-model")
        .await
        .expect("send ok");
    handle.join().expect("server");

    assert_eq!(response.model, "caller-model");
    let choice = &response.choices[0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    assert_eq!(choice["message"]["content"], Value::Null);
    assert_eq!(
        choice["message"]["tool_calls"],
        json!([{ "id": "call_9", "type": "function",
                 "function": { "name": "weather", "arguments": "{\"city\":\"Siem Reap\"}" } }])
    );
    assert_eq!(response.rest["usage"]["total_tokens"], 12);
    assert_eq!(response.rest["object"], "chat.completion");
}

#[tokio::test]
async fn a_tool_call_seen_only_at_done_still_arrives_whole_and_once() {
    let item = json!({ "type": "function_call", "id": "fc_2", "call_id": "call_2",
                       "name": "f", "arguments": "{\"a\":1}" });
    let body = sse(&[
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        completed(&json!({})),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 3);
    let response = upstream(&base)
        .send(request("m"), "m")
        .await
        .expect("send ok");
    handle.join().expect("server");
    assert_eq!(
        response.choices[0]["message"]["tool_calls"],
        json!([{ "id": "call_2", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }])
    );
}

#[tokio::test]
async fn a_stream_cut_before_completion_ends_with_an_error() {
    let body = sse(&[json!({ "type": "response.output_text.delta", "delta": "partial" })]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let chunks = all_chunks(&upstream(&base)).await;
    handle.join().expect("server");
    assert!(chunks[0].is_ok());
    match chunks.last() {
        Some(Err(ProtocolError::UpstreamProtocol(_))) => {}
        other => panic!("expected a truncation error, got {other:?}"),
    }
}

#[tokio::test]
async fn crlf_framing_and_multi_line_data_decode() {
    let body = "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\r\ndata: \"delta\":\"ok\"}\r\n\r\n\
                data: {\"type\":\"response.completed\",\"response\":{}}\r\n\r\n";
    let (base, handle) = serve_events("200 OK", body, 5);
    let chunks = all_chunks(&upstream(&base)).await;
    handle.join().expect("server");
    let chunks: Vec<ChatChunk> = chunks.into_iter().map(|c| c.expect("ok")).collect();
    assert_eq!(chunks[0].choices[0].delta["content"], "ok");
    assert_eq!(
        chunks.last().expect("last").choices[0].rest["finish_reason"],
        "stop"
    );
}

/// A bearer source that records what the backend rejected.
#[derive(Debug, Default)]
struct RecordingBearer(Mutex<Vec<String>>);

#[async_trait]
impl BearerSource for RecordingBearer {
    async fn bearer(&self) -> Result<Secret, ProtocolError> {
        Ok(Secret::new("expired".to_owned()))
    }

    async fn rejected(&self, rejected: &Secret) {
        self.0
            .lock()
            .expect("lock")
            .push(rejected.expose().to_owned());
    }
}

#[tokio::test]
async fn a_401_is_reported_to_the_bearer_source() {
    let (base, handle) = serve_events(
        "401 Unauthorized",
        "{\"error\":{\"code\":\"invalid_token\"}}",
        1,
    );
    let bearer = Arc::new(RecordingBearer::default());
    let upstream = ResponsesUpstream::new(&base, bearer.clone(), RequestPolicy::Plan);
    let outcome = upstream.stream(request("m"), "m").await;
    handle.join().expect("server");
    assert!(matches!(
        outcome,
        Err(ProtocolError::UpstreamStatus { status: 401, .. })
    ));
    assert_eq!(*bearer.0.lock().expect("lock"), vec!["expired".to_owned()]);
}

#[tokio::test]
async fn an_untranslatable_request_is_refused_before_anything_is_sent() {
    // Port 9 (discard) is never contacted: refusal happens first.
    let upstream = upstream("http://127.0.0.1:9");
    let mut req = request("m");
    req.rest.insert("n".to_owned(), json!(3));
    match upstream.send(req, "m").await {
        Err(ProtocolError::UnsupportedRequest(reason)) => assert!(reason.contains('n')),
        other => panic!("expected UnsupportedRequest, got {other:?}"),
    }
}

#[tokio::test]
async fn null_usage_stays_unknown_even_when_asked_for() {
    let body = sse(&[
        json!({ "type": "response.output_text.delta", "delta": "ok" }),
        json!({ "type": "response.incomplete", "response": { "usage": null,
                "incomplete_details": { "reason": "max_output_tokens" } } }),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let chunks: Vec<ChatChunk> = all_chunks_of(&upstream(&base), usage_request())
        .await
        .into_iter()
        .map(|item| item.expect("chunk ok"))
        .collect();
    handle.join().expect("server");
    let last = chunks.last().expect("a finish chunk");
    assert_eq!(last.choices[0].rest["finish_reason"], "length");
    assert!(chunks.iter().all(|chunk| chunk.rest["usage"].is_null()));
    assert!(
        chunks.iter().all(|chunk| !chunk.choices.is_empty()),
        "no usage chunk"
    );
}

#[tokio::test]
async fn early_text_that_the_done_arguments_contradict_fails() {
    // No output_item.added: early deltas, then a done whose arguments are not
    // an extension of them. The done arguments are authoritative.
    let body = sse(&[
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 0, "delta": "{\"x\":123}" }),
        json!({ "type": "response.output_item.done", "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "c1",
                          "name": "f", "arguments": "{\"x\":1}" } }),
        completed(&json!({})),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let outcome = upstream(&base).send(request("m"), "m").await;
    handle.join().expect("server");
    assert!(
        matches!(outcome, Err(ProtocolError::UpstreamProtocol(_))),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn argument_text_before_the_call_starts_is_kept() {
    // A reordering backend: one delta arrives before output_item.added.
    let body = sse(&[
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 0, "delta": "{\"x\":" }),
        json!({ "type": "response.output_item.added", "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "c1",
                          "name": "f", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 0, "delta": "1}" }),
        json!({ "type": "response.output_item.done", "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "c1",
                          "name": "f", "arguments": "{\"x\":1}" } }),
        completed(&json!({})),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let response = upstream(&base)
        .send(request("m"), "m")
        .await
        .expect("send ok");
    handle.join().expect("server");
    assert_eq!(
        response.choices[0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"x\":1}"
    );
}

#[tokio::test]
async fn arguments_that_contradict_the_deltas_fail_the_response() {
    let body = sse(&[
        json!({ "type": "response.output_item.added", "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "c1",
                          "name": "f", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_1",
                "output_index": 0, "delta": "{\"a\"" }),
        json!({ "type": "response.output_item.done", "output_index": 0,
                "item": { "type": "function_call", "id": "fc_1", "call_id": "c1",
                          "name": "f", "arguments": "{\"b\":2}" } }),
        completed(&json!({})),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let outcome = upstream(&base).send(request("m"), "m").await;
    handle.join().expect("server");
    assert!(
        matches!(outcome, Err(ProtocolError::UpstreamProtocol(_))),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_call_with_no_arguments_gets_an_empty_object() {
    let item = json!({ "type": "function_call", "id": "fc_1", "call_id": "c1",
                       "name": "now", "arguments": "" });
    let body = sse(&[
        json!({ "type": "response.output_item.added", "output_index": 0, "item": item }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        completed(&json!({})),
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let response = upstream(&base)
        .send(request("m"), "m")
        .await
        .expect("send ok");
    handle.join().expect("server");
    assert_eq!(
        response.choices[0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
}

#[tokio::test]
async fn cr_only_lines_and_a_leading_bom_decode() {
    let body = "\u{feff}data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\r\r\
                data: {\"type\":\"response.completed\",\"response\":{}}\r\r";
    let (base, handle) = serve_events("200 OK", body, 7);
    let chunks: Vec<ChatChunk> = all_chunks(&upstream(&base))
        .await
        .into_iter()
        .map(|item| item.expect("chunk ok"))
        .collect();
    handle.join().expect("server");
    assert_eq!(chunks[0].choices[0].delta["content"], "hi");
    assert_eq!(
        chunks.last().expect("last").choices[0].rest["finish_reason"],
        "stop"
    );
}

#[tokio::test]
async fn a_terminal_event_without_its_blank_line_is_not_a_success() {
    let body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
                data: {\"type\":\"response.completed\",\"response\":{}}\n";
    let (base, handle) = serve_events("200 OK", body, 1);
    let chunks = all_chunks(&upstream(&base)).await;
    handle.join().expect("server");
    match chunks.last() {
        Some(Err(ProtocolError::UpstreamProtocol(_))) => {}
        other => panic!("expected a truncation error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    // A 307 would resend the bearer and the prompt body to the new host.
    let (target, target_handle) = serve_events("200 OK", "", 1);
    let location = format!("307 Temporary Redirect\r\nLocation: {target}/responses");
    let (base, handle) = serve_events(&location, "", 1);
    let outcome = upstream(&base).stream(request("m"), "m").await;
    handle.join().expect("server");
    assert!(
        matches!(
            outcome,
            Err(ProtocolError::UpstreamStatus { status: 307, .. })
        ),
        "{outcome:?}"
    );
    // Release the target's accept loop; it must have seen no request.
    drop(std::net::TcpStream::connect(
        target.trim_start_matches("http://"),
    ));
    assert_eq!(target_handle.join().expect("target"), "");
}
