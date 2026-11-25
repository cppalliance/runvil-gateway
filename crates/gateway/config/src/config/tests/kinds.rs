//! Tests for model kind, thinking mode, and tool dialect parsing.

use super::super::*;
use super::SAMPLE;

#[test]
fn parses_thinking_modes() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "anthropic"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "m"
description = "prose"
context = 8192
thinking = "switchable"
upstream = "u"
endpoints = ["anthropic"]
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.models[0].thinking, ThinkingMode::Switchable);
}

#[test]
fn model_kind_defaults_to_chat() {
    let config = Config::from_toml_str(SAMPLE).unwrap();
    assert_eq!(config.models[0].kind, ModelKind::Chat);

    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[local_model]]
name = "q"
description = "prose"
source = "/models/q.gguf"
context = 4096
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.local_models[0].kind, ModelKind::Chat);
}

#[test]
fn parses_model_kinds() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "embed"
kind = "embedding"
description = "prose"
context = 8192
upstream = "u"
endpoints = ["e"]

[[local_model]]
name = "rerank"
kind = "classifier"
description = "prose"
source = "/models/r.gguf"
context = 4096
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.models[0].kind, ModelKind::Embedding);
    assert_eq!(config.local_models[0].kind, ModelKind::Classifier);
}

#[test]
fn rejects_unknown_model_kind() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "m"
kind = "rerank"
description = "prose"
context = 8192
upstream = "u"
endpoints = ["e"]
"#;
    assert!(matches!(
        Config::parse_toml(toml),
        Err(ConfigError::Parse { .. })
    ));
}

#[test]
fn tool_dialect_defaults_to_openai() {
    let config = Config::from_toml_str(SAMPLE).unwrap();
    assert_eq!(config.models[0].tool_dialect, ToolDialect::Openai);
}

#[test]
fn parses_gemma3_tool_code_dialect() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "m"
description = "prose"
context = 8192
tool_dialect = "gemma3_tool_code"
upstream = "u"
endpoints = ["e"]
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.models[0].tool_dialect, ToolDialect::Gemma3ToolCode);
}

#[test]
fn rejects_unknown_tool_dialect() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "m"
description = "prose"
context = 8192
tool_dialect = "anthropic"
upstream = "u"
endpoints = ["e"]
"#;
    assert!(matches!(
        Config::parse_toml(toml),
        Err(ConfigError::Parse { .. })
    ));
}

#[test]
fn rejects_tool_dialect_on_a_non_chat_model() {
    let toml = r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
protocol = "openai"
base_url = "http://a"
api_key = ""

[[model]]
name = "m"
kind = "embedding"
description = "prose"
context = 8192
tool_dialect = "gemma3_tool_code"
upstream = "u"
endpoints = ["e"]
"#;
    assert!(matches!(
        Config::parse_toml(toml),
        Err(ConfigError::Validation(_))
    ));
}

/// A config with one `openai-responses` endpoint and one model on it, with
/// `model_lines` added to the model.
fn responses_config(model_lines: &str) -> String {
    format!(
        r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "r"
protocol = "openai-responses"
base_url = "https://api.openai.com/v1"
api_key = "k"

[[model]]
name = "m"
description = "prose"
context = 8192
upstream = "u"
endpoints = ["r"]
{model_lines}
"#
    )
}

#[test]
fn a_chat_model_on_a_responses_endpoint_parses() {
    let config = Config::from_toml_str(&responses_config("")).unwrap();
    assert_eq!(config.endpoints[0].protocol, Protocol::OpenaiResponses);
}

#[test]
fn a_responses_endpoint_refuses_non_chat_kinds_and_emulated_dialects() {
    for (lines, needle) in [
        (r#"kind = "embedding""#, "serves chat only"),
        (
            r#"tool_dialect = "gemma3_tool_code""#,
            "tool_dialect openai",
        ),
    ] {
        match Config::parse_toml(&responses_config(lines)) {
            Err(ConfigError::Validation(message)) => assert!(message.contains(needle), "{message}"),
            other => panic!("expected a validation error for {lines}, got {other:?}"),
        }
    }
}
