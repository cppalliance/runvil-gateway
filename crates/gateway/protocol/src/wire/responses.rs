//! Chat completions to OpenAI Responses: the request half of the
//! `openai-responses` endpoint protocol.
//!
//! Callers always speak chat completions to the gateway. An endpoint whose
//! protocol is `openai-responses` receives the same request rewritten as a
//! Responses request, built here. The mapping:
//!
//! | Chat completions | Responses |
//! |---|---|
//! | leading `system` / `developer` messages | `instructions` |
//! | later `system` / `developer` messages | `developer` message items |
//! | `user` / `assistant` text | message items |
//! | assistant `tool_calls` | `function_call` items |
//! | `tool` messages | `function_call_output` items |
//! | `tools` (`function` wrapper) | flat `function` tools |
//! | `tool_choice`, `parallel_tool_calls` | the same |
//! | `reasoning_effort` | `reasoning.effort` |
//! | `response_format` | `text.format` |
//! | `temperature`, `top_p`, `max_tokens` / `max_completion_tokens` | the same, `max_output_tokens` (standard policy only) |
//!
//! The upstream request always streams and never asks the provider to
//! store the response. Fields with no Responses equivalent that only tune
//! the reply (`stream_options`, `stop`, `seed`, `chat_template_kwargs`, …)
//! are dropped; fields whose loss would change what the caller gets back
//! (`n` above 1, legacy `function` messages, non-function tools) refuse the
//! request before anything is sent.

use serde_json::{Map, Value, json};

use super::ChatRequest;

/// Which request fields an `openai-responses` endpoint accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestPolicy {
    /// An endpoint reached with an API key: sampling and output-limit
    /// fields pass through under their Responses names.
    Standard,
    /// A ChatGPT plan endpoint, which refuses sampling and output-limit
    /// fields (`temperature`, `top_p`, `max_output_tokens`, …), so they are
    /// never sent.
    Plan,
}

/// Builds the Responses request body for `req`, addressed to
/// `upstream_model`.
///
/// # Errors
/// Returns a static reason when `req` holds something the Responses shape
/// cannot express without changing the reply: `n` above 1, a legacy
/// `function` message or `function_call`, a tool that is not a function, a
/// content part that is neither text nor an image, a tool call or tool
/// result missing its id, or messages that are all system or developer.
pub fn to_responses_request(
    req: &ChatRequest,
    upstream_model: &str,
    policy: RequestPolicy,
) -> Result<Value, &'static str> {
    if req
        .rest
        .get("n")
        .and_then(Value::as_u64)
        .is_some_and(|n| n > 1)
    {
        return Err("n above 1 has no Responses equivalent");
    }
    let (instructions, input) = translate_messages(&req.messages)?;
    if input.is_empty() {
        return Err("a Responses request needs a user, assistant or tool message");
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::String(upstream_model.to_owned()));
    if !instructions.is_empty() {
        body.insert(
            "instructions".into(),
            Value::String(instructions.join("\n\n")),
        );
    }
    body.insert("input".into(), Value::Array(input));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));

    if let Some(tools) = req.rest.get("tools").and_then(Value::as_array)
        && !tools.is_empty()
    {
        let tools = tools
            .iter()
            .map(translate_tool)
            .collect::<Result<Vec<_>, _>>()?;
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = req.rest.get("tool_choice") {
        body.insert("tool_choice".into(), translate_tool_choice(choice)?);
    }
    if let Some(parallel) = req.rest.get("parallel_tool_calls") {
        body.insert("parallel_tool_calls".into(), parallel.clone());
    }
    if let Some(effort) = req.rest.get("reasoning_effort").and_then(Value::as_str) {
        body.insert("reasoning".into(), json!({ "effort": effort }));
    }
    if let Some(format) = req.rest.get("response_format") {
        body.insert(
            "text".into(),
            json!({ "format": translate_response_format(format)? }),
        );
    }
    if policy == RequestPolicy::Standard {
        for key in ["temperature", "top_p"] {
            if let Some(value) = req.rest.get(key) {
                body.insert(key.into(), value.clone());
            }
        }
        let limit = req
            .rest
            .get("max_completion_tokens")
            .or_else(|| req.rest.get("max_tokens"));
        if let Some(limit) = limit {
            body.insert("max_output_tokens".into(), limit.clone());
        }
    }
    Ok(Value::Object(body))
}

/// Splits chat messages into the Responses `instructions` (the leading run
/// of system and developer messages) and the `input` items.
fn translate_messages(messages: &[Value]) -> Result<(Vec<String>, Vec<Value>), &'static str> {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut leading = true;
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let content = message.get("content").unwrap_or(&Value::Null);
        match role {
            "system" | "developer" => {
                let text = joined_text(content)?;
                if leading {
                    instructions.push(text);
                } else {
                    input.push(json!({ "type": "message", "role": "developer", "content": text }));
                }
            }
            "user" => {
                leading = false;
                let content = user_content(content)?;
                input.push(json!({ "type": "message", "role": "user", "content": content }));
            }
            "assistant" => {
                leading = false;
                if message.get("function_call").is_some() {
                    return Err("legacy function_call messages have no Responses equivalent");
                }
                let text = joined_text(content)?;
                if !text.is_empty() {
                    input.push(json!({ "type": "message", "role": "assistant", "content": text }));
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        input.push(function_call_item(call)?);
                    }
                }
            }
            "tool" => {
                leading = false;
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or("a tool message has no tool_call_id")?;
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": joined_text(content)?,
                }));
            }
            _ => return Err("legacy function messages have no Responses equivalent"),
        }
    }
    Ok((instructions, input))
}

/// The text of a message's content: a string as is, a parts array as its
/// text parts joined, null as empty. Any non-text part is refused, because
/// only user messages may carry images.
fn joined_text(content: &Value) -> Result<String, &'static str> {
    match content {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text" | "refusal") => {
                        let piece = part
                            .get("text")
                            .or_else(|| part.get("refusal"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        text.push_str(piece);
                    }
                    _ => return Err("only user messages may hold non-text content parts"),
                }
            }
            Ok(text)
        }
        _ => Err("message content must be a string, an array of parts, or null"),
    }
}

/// A user message's content: a string as is, or parts mapped to
/// `input_text` and `input_image`.
fn user_content(content: &Value) -> Result<Value, &'static str> {
    let Value::Array(parts) = content else {
        return Ok(Value::String(joined_text(content)?));
    };
    let mut mapped = Vec::with_capacity(parts.len());
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                mapped.push(json!({ "type": "input_text", "text": text }));
            }
            Some("image_url") => {
                let image = part.get("image_url").unwrap_or(&Value::Null);
                let url = image
                    .get("url")
                    .or(Some(image))
                    .and_then(Value::as_str)
                    .ok_or("an image_url part has no url")?;
                let detail = image
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("auto");
                mapped.push(json!({ "type": "input_image", "image_url": url, "detail": detail }));
            }
            _ => return Err("only text and image_url content parts have a Responses equivalent"),
        }
    }
    Ok(Value::Array(mapped))
}

/// One chat `tool_calls` entry as a Responses `function_call` item.
fn function_call_item(call: &Value) -> Result<Value, &'static str> {
    let call_id = call
        .get("id")
        .and_then(Value::as_str)
        .ok_or("a tool call has no id")?;
    let function = call.get("function").ok_or("a tool call has no function")?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a tool call has no function name")?;
    let arguments = match function.get("arguments") {
        Some(Value::String(arguments)) => arguments.clone(),
        None | Some(Value::Null) => "{}".to_owned(),
        Some(other) => other.to_string(),
    };
    Ok(json!({ "type": "function_call", "call_id": call_id, "name": name, "arguments": arguments }))
}

/// A chat `{type: function, function: {...}}` tool as a flat Responses
/// function tool. `strict` defaults to false, because a chat client's
/// schema need not meet strict mode's rules.
fn translate_tool(tool: &Value) -> Result<Value, &'static str> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return Err("only function tools have a Responses equivalent here");
    }
    let function = tool
        .get("function")
        .ok_or("a function tool has no function")?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a function tool has no name")?;
    let mut flat = Map::new();
    flat.insert("type".into(), Value::String("function".into()));
    flat.insert("name".into(), Value::String(name.to_owned()));
    if let Some(description) = function.get("description") {
        flat.insert("description".into(), description.clone());
    }
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    flat.insert("parameters".into(), parameters);
    let strict = function
        .get("strict")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    flat.insert("strict".into(), Value::Bool(strict));
    Ok(Value::Object(flat))
}

/// `tool_choice`: the string modes pass through; a named function loses
/// the chat `function` wrapper.
fn translate_tool_choice(choice: &Value) -> Result<Value, &'static str> {
    match choice {
        Value::String(_) => Ok(choice.clone()),
        Value::Object(_) => {
            let name = choice
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .ok_or("tool_choice names no function")?;
            Ok(json!({ "type": "function", "name": name }))
        }
        _ => Err("tool_choice must be a string or a named function"),
    }
}

/// `response_format` as a Responses `text.format`: `json_schema` loses its
/// wrapper object; `text` and `json_object` pass through.
fn translate_response_format(format: &Value) -> Result<Value, &'static str> {
    match format.get("type").and_then(Value::as_str) {
        Some("text" | "json_object") => Ok(format.clone()),
        Some("json_schema") => {
            let schema = format
                .get("json_schema")
                .and_then(Value::as_object)
                .ok_or("a json_schema response_format has no json_schema")?;
            let mut flat = schema.clone();
            flat.insert("type".into(), Value::String("json_schema".into()));
            Ok(Value::Object(flat))
        }
        _ => Err("response_format must be text, json_object or json_schema"),
    }
}

#[cfg(test)]
mod tests;
