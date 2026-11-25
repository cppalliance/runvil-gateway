//! Tests for the chat completions to Responses request translation.

use serde_json::{Map, Value, json};

use super::*;

fn chat(body: Value) -> ChatRequest {
    serde_json::from_value(body).expect("a chat request")
}

fn translate(body: Value, policy: RequestPolicy) -> Value {
    to_responses_request(&chat(body), "upstream-model", policy).expect("translates")
}

#[test]
fn every_request_streams_unstored_to_the_upstream_model() {
    let out = translate(
        json!({ "model": "caller", "messages": [{ "role": "user", "content": "hi" }] }),
        RequestPolicy::Standard,
    );
    assert_eq!(out["model"], "upstream-model");
    assert_eq!(out["stream"], true);
    assert_eq!(out["store"], false);
    assert_eq!(
        out["input"],
        json!([{ "type": "message", "role": "user", "content": "hi" }])
    );
    assert!(out.get("instructions").is_none());
}

#[test]
fn leading_system_messages_become_instructions_and_later_ones_developer_items() {
    let out = translate(
        json!({ "model": "m", "messages": [
            { "role": "system", "content": "Be brief." },
            { "role": "developer", "content": [{ "type": "text", "text": "Cite." }] },
            { "role": "user", "content": "q" },
            { "role": "system", "content": "Now in French." },
        ]}),
        RequestPolicy::Standard,
    );
    assert_eq!(out["instructions"], "Be brief.\n\nCite.");
    assert_eq!(
        out["input"][1],
        json!({ "type": "message", "role": "developer", "content": "Now in French." })
    );
}

#[test]
fn a_tool_round_maps_to_function_call_items() {
    // The shape the harness sends after a tool round: an assistant turn with
    // tool_calls and null content, then the tool's result.
    let out = translate(
        json!({ "model": "m", "messages": [
            { "role": "user", "content": "weather?" },
            { "role": "assistant", "content": null, "tool_calls": [
                { "id": "call_1", "type": "function",
                  "function": { "name": "weather", "arguments": "{\"city\":\"Siem Reap\"}" } }
            ]},
            { "role": "tool", "tool_call_id": "call_1", "content": "31C" },
        ]}),
        RequestPolicy::Standard,
    );
    assert_eq!(
        out["input"],
        json!([
            { "type": "message", "role": "user", "content": "weather?" },
            { "type": "function_call", "call_id": "call_1", "name": "weather",
              "arguments": "{\"city\":\"Siem Reap\"}" },
            { "type": "function_call_output", "call_id": "call_1", "output": "31C" },
        ])
    );
}

#[test]
fn assistant_text_with_tool_calls_keeps_both_in_order() {
    let out = translate(
        json!({ "model": "m", "messages": [
            { "role": "user", "content": "q" },
            { "role": "assistant", "content": "Checking.", "tool_calls": [
                { "id": "c", "type": "function", "function": { "name": "f", "arguments": {} } }
            ]},
            { "role": "tool", "tool_call_id": "c", "content": [{ "type": "text", "text": "ok" }] },
        ]}),
        RequestPolicy::Standard,
    );
    assert_eq!(
        out["input"][1],
        json!({ "type": "message", "role": "assistant", "content": "Checking." })
    );
    assert_eq!(out["input"][2]["arguments"], "{}");
    assert_eq!(out["input"][3]["output"], "ok");
}

#[test]
fn user_parts_map_to_input_text_and_input_image() {
    let out = translate(
        json!({ "model": "m", "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "What is this?" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA", "detail": "low" } },
            { "type": "image_url", "image_url": { "url": "https://example.test/a.png" } },
        ]}]}),
        RequestPolicy::Plan,
    );
    assert_eq!(
        out["input"][0]["content"],
        json!([
            { "type": "input_text", "text": "What is this?" },
            { "type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "low" },
            { "type": "input_image", "image_url": "https://example.test/a.png", "detail": "auto" },
        ])
    );
}

#[test]
fn harness_tools_flatten_and_default_to_non_strict() {
    // The harness's request body: function tools in the chat wrapper,
    // tool_choice auto, include_usage, and llama.cpp's template kwargs.
    let out = translate(
        json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "stream": true,
            "stream_options": { "include_usage": true },
            "tools": [{ "type": "function", "function": {
                "name": "read_file", "description": "Read a file.",
                "parameters": { "type": "object", "properties": { "path": { "type": "string" } } }
            }}],
            "tool_choice": "auto",
            "chat_template_kwargs": { "enable_thinking": true },
        }),
        RequestPolicy::Standard,
    );
    assert_eq!(
        out["tools"],
        json!([{ "type": "function", "name": "read_file", "description": "Read a file.",
                 "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
                 "strict": false }])
    );
    assert_eq!(out["tool_choice"], "auto");
    for dropped in ["stream_options", "chat_template_kwargs", "messages"] {
        assert!(
            out.get(dropped).is_none(),
            "{dropped} must not reach the upstream"
        );
    }
}

#[test]
fn named_tool_choice_reasoning_and_json_schema_translate() {
    let out = translate(
        json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": { "type": "function", "function": { "name": "f" } },
            "reasoning_effort": "low",
            "response_format": { "type": "json_schema", "json_schema": {
                "name": "answer", "strict": true, "schema": { "type": "object" }
            }},
        }),
        RequestPolicy::Plan,
    );
    assert_eq!(
        out["tool_choice"],
        json!({ "type": "function", "name": "f" })
    );
    assert_eq!(out["reasoning"], json!({ "effort": "low" }));
    assert_eq!(
        out["text"],
        json!({ "format": { "type": "json_schema", "name": "answer", "strict": true,
                            "schema": { "type": "object" } } })
    );
}

#[test]
fn standard_policy_passes_sampling_and_limits_and_plan_policy_drops_them() {
    let body = json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "hi" }],
        "temperature": 0.2,
        "top_p": 0.9,
        "max_tokens": 64,
    });
    let standard = translate(body.clone(), RequestPolicy::Standard);
    assert_eq!(standard["temperature"], 0.2);
    assert_eq!(standard["top_p"], 0.9);
    assert_eq!(standard["max_output_tokens"], 64);

    let plan = translate(body, RequestPolicy::Plan);
    for refused in ["temperature", "top_p", "max_output_tokens", "max_tokens"] {
        assert!(
            plan.get(refused).is_none(),
            "{refused} must not reach a plan endpoint"
        );
    }
}

#[test]
fn max_completion_tokens_wins_over_max_tokens() {
    let out = translate(
        json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }],
                "max_tokens": 10, "max_completion_tokens": 20 }),
        RequestPolicy::Standard,
    );
    assert_eq!(out["max_output_tokens"], 20);
}

#[test]
fn requests_with_no_responses_equivalent_are_refused() {
    let user = json!({ "role": "user", "content": "hi" });
    let cases = [
        json!({ "model": "m", "messages": [user], "n": 2 }),
        json!({ "model": "m", "messages": [{ "role": "function", "name": "f", "content": "x" }] }),
        json!({ "model": "m", "messages": [user, { "role": "assistant", "content": null,
                "function_call": { "name": "f", "arguments": "{}" } }] }),
        json!({ "model": "m", "messages": [user], "tools": [{ "type": "code_interpreter" }] }),
        json!({ "model": "m", "messages": [{ "role": "user", "content": [
                { "type": "input_audio", "input_audio": { "data": "AA", "format": "wav" } }] }] }),
        json!({ "model": "m", "messages": [user, { "role": "tool", "content": "x" }] }),
        json!({ "model": "m", "messages": [{ "role": "system", "content": "only rules" }] }),
        json!({ "model": "m", "messages": [{ "role": "system", "content": [
                { "type": "image_url", "image_url": { "url": "x" } }] }, user] }),
    ];
    for case in cases {
        let request = chat(case.clone());
        assert!(
            to_responses_request(&request, "u", RequestPolicy::Standard).is_err(),
            "expected a refusal for {case}"
        );
    }
}

#[test]
fn n_of_one_is_accepted() {
    let out = translate(
        json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }], "n": 1 }),
        RequestPolicy::Standard,
    );
    assert!(out.get("n").is_none());
    let _: &Map<String, Value> = out.as_object().expect("an object");
}

/// A conversation as a Harness sends it: each request is the one before
/// plus new messages. The tool-call arguments are deliberately untidy.
fn growing_conversation() -> Vec<Value> {
    let untidy = "{ \"b\":1,\n  \"a\" : [2, 3] }";
    vec![
        json!({ "role": "system", "content": "Be brief." }),
        json!({ "role": "user", "content": "Compare the two." }),
        json!({ "role": "assistant", "content": "Looking both up.", "tool_calls": [
            { "id": "call_1", "type": "function", "function": { "name": "look_up", "arguments": untidy } },
            { "id": "call_2", "type": "function", "function": { "name": "look_up", "arguments": "{\"a\":4}" } },
        ]}),
        json!({ "role": "tool", "tool_call_id": "call_1", "content": "one" }),
        json!({ "role": "tool", "tool_call_id": "call_2", "content": "two" }),
        json!({ "role": "assistant", "content": "The first is smaller." }),
        json!({ "role": "system", "content": "Now in French." }),
        json!({ "role": "user", "content": "And the third?" }),
        json!({ "role": "assistant", "content": null, "tool_calls": [
            { "id": "call_3", "type": "function", "function": { "name": "look_up", "arguments": "{}" } },
        ]}),
        json!({ "role": "tool", "tool_call_id": "call_3", "content": "three" }),
        json!({ "role": "assistant", "content": "Le troisième est le plus grand." }),
    ]
}

/// A provider caches a request by its exact prefix. So a history that
/// extends another must translate to a request that extends the other's:
/// the same instructions and tools, and an `input` that only grows at the
/// end. Otherwise every round would miss the cache from the first changed
/// item on.
#[test]
fn a_longer_history_translates_to_an_extension_of_the_shorter_one() {
    let conversation = growing_conversation();
    let tools = json!([{ "type": "function", "function": {
        "name": "look_up", "parameters": { "type": "object", "properties": {} }
    }}]);
    for policy in [RequestPolicy::Standard, RequestPolicy::Plan] {
        let request = |length: usize| {
            translate(
                json!({ "model": "m", "tools": tools, "messages": conversation[..length] }),
                policy,
            )
        };
        // Every history from the first user message on.
        for length in 2..conversation.len() {
            let (shorter, longer) = (request(length), request(length + 1));
            let (before, after) = (
                shorter["input"].as_array().expect("input"),
                longer["input"].as_array().expect("input"),
            );
            assert!(after.len() >= before.len(), "history of {length}");
            assert_eq!(&after[..before.len()], &before[..], "history of {length}");
            assert_eq!(shorter["instructions"], longer["instructions"]);
            assert_eq!(shorter["tools"], longer["tools"]);
        }
        // The same request translates to the same bytes every time.
        let whole = conversation.len();
        assert_eq!(request(whole).to_string(), request(whole).to_string());
    }
}

/// Tool-call arguments go back to the provider exactly as it wrote them:
/// re-rendering the JSON would change the prefix it cached.
#[test]
fn tool_call_arguments_are_passed_through_byte_for_byte() {
    let out = translate(
        json!({ "model": "m", "messages": growing_conversation() }),
        RequestPolicy::Standard,
    );
    let arguments: Vec<&str> = out["input"]
        .as_array()
        .expect("input")
        .iter()
        .filter(|item| item["type"] == "function_call")
        .filter_map(|item| item["arguments"].as_str())
        .collect();
    assert_eq!(
        arguments,
        ["{ \"b\":1,\n  \"a\" : [2, 3] }", "{\"a\":4}", "{}"]
    );
}
