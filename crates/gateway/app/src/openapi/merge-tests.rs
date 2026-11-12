//! Merging two responses that share a status.

use serde_json::{Map, Value, json};

use super::merge_response;

fn merged(first: Value, second: Value) -> Result<Value, String> {
    let mut responses = Map::new();
    merge_response(&mut responses, "403".to_owned(), first)?;
    merge_response(&mut responses, "403".to_owned(), second)?;
    Ok(responses.remove("403").expect("the merged response"))
}

#[test]
fn a_lone_response_is_inserted_unchanged() {
    let response = json!({ "description": "one", "x-runvil-source": "a.rs#one" });
    let mut responses = Map::new();
    merge_response(&mut responses, "403".to_owned(), response.clone()).expect("an insert");
    assert_eq!(responses["403"], response);
}

#[test]
fn two_refusals_keep_both_descriptions_and_both_sources() {
    let response = merged(
        json!({ "description": "wall", "x-runvil-source": "host.rs#wall" }),
        json!({ "description": "origin", "x-runvil-source": "auth.rs#origin" }),
    )
    .expect("a merge");
    assert_eq!(
        response,
        json!({
            "description": "wall\n\norigin",
            "x-runvil-source": ["host.rs#wall", "auth.rs#origin"]
        })
    );
}

#[test]
fn a_repeated_source_stays_a_single_string() {
    let response = merged(
        json!({ "description": "a", "x-runvil-source": "same.rs#item" }),
        json!({ "description": "b", "x-runvil-source": "same.rs#item" }),
    )
    .expect("a merge");
    assert_eq!(response["x-runvil-source"], "same.rs#item");
}

#[test]
fn error_codes_and_types_are_a_sorted_union() {
    let response = merged(
        json!({
            "description": "a",
            "x-runvil-error-codes": ["model_not_found"],
            "x-runvil-error-types": ["invalid_request_error"]
        }),
        json!({
            "description": "b",
            "x-runvil-error-codes": ["malformed_request", "model_not_found"],
            "x-runvil-error-types": ["invalid_request_error"]
        }),
    )
    .expect("a merge");
    assert_eq!(
        response["x-runvil-error-codes"],
        json!(["malformed_request", "model_not_found"])
    );
    assert_eq!(
        response["x-runvil-error-types"],
        json!(["invalid_request_error"])
    );
}

#[test]
fn a_body_on_one_side_is_kept() {
    let content = json!({ "application/json": { "schema": { "type": "object" } } });
    let response = merged(
        json!({ "description": "a" }),
        json!({ "content": content, "description": "b" }),
    )
    .expect("a merge");
    assert_eq!(response["content"], content);
}

#[test]
fn two_different_bodies_for_one_status_are_refused() {
    let error = merged(
        json!({ "content": { "application/json": {} }, "description": "a" }),
        json!({ "content": { "text/plain": {} }, "description": "b" }),
    )
    .expect_err("two bodies conflict");
    assert!(
        error.contains("403") && error.contains("`content`"),
        "{error}"
    );
}
