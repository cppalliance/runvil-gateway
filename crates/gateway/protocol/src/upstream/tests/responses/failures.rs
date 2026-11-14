//! Tests for how a failure the upstream reports inside its event stream
//! reaches the caller.

use serde_json::{Value, json};

use super::*;

/// The last item of a stream that reports `failure` after one text delta.
async fn last_after(failure: Value) -> Option<Result<ChatChunk, ProtocolError>> {
    let body = sse(&[
        json!({ "type": "response.output_text.delta", "delta": "Hel" }),
        failure,
    ]);
    let (base, handle) = serve_events("200 OK", &body, 1);
    let mut chunks = all_chunks(&upstream(&base)).await;
    handle.join().expect("server");
    chunks.pop()
}

/// Asserts `last` is an upstream failure that displays as `display`, which
/// names both the status and the code.
fn assert_failure(last: Option<Result<ChatChunk, ProtocolError>>, display: &str) {
    match last {
        Some(Err(error @ ProtocolError::UpstreamStatus { .. })) => {
            assert_eq!(error.to_string(), display);
        }
        other => panic!("expected an upstream status, got {other:?}"),
    }
}

#[tokio::test]
async fn a_usage_limit_failure_mid_stream_is_a_429() {
    let last = last_after(json!({ "type": "response.failed", "response": { "error": {
        "code": "subscription_sharing_usage_limit_exceeded",
        "message": "You have reached your plan's limit." } } }))
    .await;
    assert_failure(
        last,
        "upstream returned 429 (subscription_sharing_usage_limit_exceeded)",
    );
}

#[tokio::test]
async fn an_error_event_with_a_nested_error_keeps_its_code() {
    // The event the ChatGPT plan route sends once the plan's limit is spent.
    let last = last_after(json!({ "type": "error", "sequence_number": 3, "error": {
        "type": "invalid_request_error",
        "code": "subscription_sharing_usage_limit_exceeded",
        "message": "The ChatGPT user has reached their Subscription Sharing usage limit.",
        "param": null } }))
    .await;
    assert_failure(
        last,
        "upstream returned 429 (subscription_sharing_usage_limit_exceeded)",
    );
}

#[tokio::test]
async fn an_error_event_with_its_own_fields_keeps_its_code() {
    let last = last_after(json!({ "type": "error", "sequence_number": 1,
        "code": "rate_limit_exceeded", "message": "Slow down.", "param": null }))
    .await;
    assert_failure(last, "upstream returned 429 (rate_limit_exceeded)");
}

#[tokio::test]
async fn a_nested_invalid_request_that_names_no_limit_is_a_400() {
    let last = last_after(json!({ "type": "error", "error": {
        "type": "invalid_request_error",
        "code": "unsupported_parameter",
        "message": "Unsupported parameter." } }))
    .await;
    assert_failure(last, "upstream returned 400 (unsupported_parameter)");
}

#[tokio::test]
async fn a_nested_error_is_read_whole_and_the_events_own_fields_are_not_mixed_in() {
    let last = last_after(json!({ "type": "error",
        "code": "rate_limit_exceeded", "message": "Slow down.",
        "error": {
            "type": "invalid_request_error",
            "code": "model_not_found",
            "message": "No such model." } }))
    .await;
    assert_failure(last, "upstream returned 400 (model_not_found)");
}

#[tokio::test]
async fn an_error_field_that_is_not_an_object_leaves_the_events_own_fields() {
    for not_an_object in [json!(null), json!("text"), json!(["a"])] {
        let last = last_after(json!({ "type": "error",
            "code": "rate_limit_exceeded", "message": "Slow down.",
            "error": not_an_object }))
        .await;
        assert_failure(last, "upstream returned 429 (rate_limit_exceeded)");
    }
}

#[tokio::test]
async fn an_empty_nested_error_is_a_502_with_no_code() {
    let last = last_after(json!({ "type": "error",
        "code": "rate_limit_exceeded", "message": "Slow down.", "error": {} }))
    .await;
    assert_failure(last, "upstream returned 502");
}
