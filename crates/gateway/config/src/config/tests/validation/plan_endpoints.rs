//! Tests for `auth = "chatgpt-plan"` endpoints: what they must not set, and
//! what every other endpoint still must.

use super::super::super::*;

/// A config with one endpoint made of `endpoint_lines` and one chat model
/// on it.
fn config_with(endpoint_lines: &str) -> String {
    format!(
        r#"
config-version = 0
[server]
bind = "127.0.0.1:8081"
api_key = "t"

[[endpoint]]
id = "e"
{endpoint_lines}

[[model]]
name = "m"
description = "prose"
context = 8192
upstream = "u"
endpoints = ["e"]
"#
    )
}

const PLAN: &str = "protocol = \"openai-responses\"\nauth = \"chatgpt-plan\"";

fn refusal(endpoint_lines: &str) -> String {
    match Config::from_toml_str(&config_with(endpoint_lines)) {
        Ok(_) => panic!("must be refused: {endpoint_lines}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_plan_endpoint_parses_with_no_destination_and_no_key() {
    let config = Config::from_toml_str(&config_with(PLAN)).expect("parses");
    let endpoint = &config.endpoints()[0];
    assert_eq!(endpoint.auth(), EndpointAuth::ChatgptPlan);
    assert_eq!(endpoint.base_url(), "");
    assert!(endpoint.api_key().is_none());
}

#[test]
fn a_plan_endpoint_refuses_any_destination_or_key() {
    for (line, name) in [
        // Presence is refused, whatever the value: the right URL, an empty
        // string and the redaction marker included.
        (r#"base_url = "https://api.openai.com/v1""#, "base_url"),
        (r#"base_url = "https://elsewhere.example/v1""#, "base_url"),
        (r#"base_url = """#, "base_url"),
        (r#"api_key = "k""#, "api_key"),
        (r#"api_key = """#, "api_key"),
        (r#"api_key = "***""#, "api_key"),
    ] {
        let message = refusal(&format!("{PLAN}\n{line}"));
        assert!(
            message.contains(&format!("must not set {name}")),
            "{line}: {message}"
        );
    }
}

#[test]
fn a_plan_endpoint_needs_the_responses_protocol() {
    let message = refusal("protocol = \"openai\"\nauth = \"chatgpt-plan\"");
    assert!(
        message.contains("needs protocol = \"openai-responses\""),
        "{message}"
    );
}

#[test]
fn an_ordinary_endpoint_still_needs_both() {
    let message = refusal("protocol = \"openai\"\napi_key = \"k\"");
    assert!(message.contains("needs base_url"), "{message}");
    let message = refusal("protocol = \"openai\"\nbase_url = \"https://b.example/v1\"");
    assert!(message.contains("needs api_key"), "{message}");

    // An empty key stays allowed, and the default may be written out.
    for lines in [
        "protocol = \"openai\"\nbase_url = \"https://b.example/v1\"\napi_key = \"\"",
        "protocol = \"openai\"\nauth = \"api-key\"\nbase_url = \"https://b.example/v1\"\napi_key = \"k\"",
    ] {
        let config = Config::from_toml_str(&config_with(lines)).expect("parses");
        assert_eq!(config.endpoints()[0].auth(), EndpointAuth::ApiKey);
    }
}

#[test]
fn an_unknown_auth_is_refused() {
    let lines = "protocol = \"openai-responses\"\nauth = \"chatgpt\"";
    assert!(Config::from_toml_str(&config_with(lines)).is_err());
}

#[test]
fn the_json_shape_keeps_presence() {
    let plan = Config::from_toml_str(&config_with(PLAN))
        .expect("parses")
        .to_json();
    let endpoint = plan["endpoint"][0].as_object().expect("an endpoint");
    assert_eq!(endpoint["auth"], "chatgpt-plan");
    assert!(!endpoint.contains_key("base_url") && !endpoint.contains_key("api_key"));

    let lines = "protocol = \"openai\"\nbase_url = \"https://b.example/v1\"\napi_key = \"k\"";
    let ordinary = Config::from_toml_str(&config_with(lines))
        .expect("parses")
        .to_json();
    let endpoint = ordinary["endpoint"][0].as_object().expect("an endpoint");
    assert!(!endpoint.contains_key("auth"));
    assert_eq!(endpoint["base_url"], "https://b.example/v1");
    assert_eq!(endpoint["api_key"], "***");
}
