//! Route responses and error envelopes checked against the document's schemas.
//!
//! A status the document lists and no case produces is printed. The gap stays
//! visible instead of being failed or ignored.

use std::collections::BTreeSet;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt as _;

use super::errors;
use super::tests::generated;

type Case = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

#[test]
fn responses_match_the_document() {
    let document = generated();
    let mut exercised = BTreeSet::new();
    // Envelope sampling starts its own runtime, so it runs before the probe runtime.
    check_envelopes(&document);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(check_router(&document, &mut exercised));
    let mut quiet = true;
    for (method, path, status) in documented_statuses(&document) {
        if exercised.contains(&(method.clone(), path.clone(), status.clone())) {
            continue;
        }
        quiet = false;
        println!("unexercised {method} {path} {status}");
    }
    if quiet {
        println!("every documented status was exercised");
    }
}

fn check_envelopes(document: &Value) {
    let schema = &document["components"]["schemas"]["ErrorEnvelope"];
    for variant in errors::all_variants() {
        let (_status, body) =
            errors::envelope_body(*variant).unwrap_or_else(|error| panic!("{error}"));
        assert_matches(schema, &body, &format!("{variant:?} envelope"));
    }
}

async fn check_router(document: &Value, exercised: &mut BTreeSet<(String, String, String)>) {
    let (_temp, state) = crate::test_support::walled_fixture();
    let peer: std::net::SocketAddr = "127.0.0.1:50000".parse().expect("a socket address");
    for case in cases() {
        probe(document, exercised, &state, peer, case).await;
    }
}

fn cases() -> [Case; 8] {
    [
        ("GET", "/health", "/health", Some("Bearer test-token"), None),
        (
            "GET",
            "/v1/models",
            "/v1/models",
            Some("Bearer test-token"),
            None,
        ),
        (
            "GET",
            "/v1/audio/voices",
            "/v1/audio/voices",
            Some("Bearer test-token"),
            None,
        ),
        (
            "GET",
            "/v1/cache",
            "/v1/cache",
            Some("Bearer test-token"),
            None,
        ),
        (
            "POST",
            "/v1/chat/completions",
            "/v1/chat/completions",
            None,
            None,
        ),
        (
            "POST",
            "/v1/chat/completions",
            "/v1/chat/completions",
            Some("Bearer test-token"),
            Some("{}"),
        ),
        (
            "DELETE",
            "/v1/cache/{sha256}",
            "/v1/cache/not-a-digest",
            Some("Bearer test-token"),
            None,
        ),
        (
            "POST",
            "/v1/cache",
            "/v1/cache",
            Some("Bearer test-token"),
            Some("{}"),
        ),
    ]
}

async fn probe(
    document: &Value,
    exercised: &mut BTreeSet<(String, String, String)>,
    state: &crate::AppState,
    peer: std::net::SocketAddr,
    (method, template, path, auth, body): Case,
) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        builder = builder.header(AUTHORIZATION, auth);
    }
    if body.is_some() {
        builder = builder.header(CONTENT_TYPE, "application/json");
    }
    let mut request = builder
        .body(Body::from(body.unwrap_or("").to_owned()))
        .expect("request builds");
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = crate::build_router(state.clone(), None)
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let content_type = match response.headers().get(CONTENT_TYPE) {
        None => String::new(),
        Some(value) => value
            .to_str()
            .unwrap_or_else(|error| panic!("{method} {path}: content type: {error}"))
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned(),
    };
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|error| panic!("{method} {path}: {error}"));
    let key = status.as_u16().to_string();
    exercised.insert((
        method.to_ascii_lowercase(),
        template.to_owned(),
        key.clone(),
    ));
    if content_type == "application/json" {
        let instance: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("{method} {template} {status}: {error}"));
        let Some(schema) = response_schema(document, template, method, &key, "application/json")
        else {
            panic!("{method} {template} {status} returned JSON the document does not describe");
        };
        assert_matches(&schema, &instance, &format!("{method} {template} {status}"));
    } else if status == StatusCode::OK {
        panic!("{method} {path} 200 was {content_type}, not JSON");
    }
}

fn response_schema(
    document: &Value,
    path: &str,
    method: &str,
    status: &str,
    media: &str,
) -> Option<Value> {
    let schema =
        document["paths"][path][method.to_ascii_lowercase()]["responses"][status]["content"][media]
            ["schema"]
            .clone();
    if schema.is_null() {
        None
    } else {
        Some(resolve(document, &schema))
    }
}

fn resolve(document: &Value, schema: &Value) -> Value {
    let mut schema = schema.clone();
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unsupported $ref {reference}"));
        let mut resolved = document["components"]["schemas"][name].clone();
        if let (Some(extra), Some(object)) = (schema.as_object(), resolved.as_object_mut()) {
            for (key, value) in extra {
                if key != "$ref" {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        schema = resolved;
    }
    match &mut schema {
        Value::Object(object) => {
            for child in object.values_mut() {
                *child = resolve(document, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = resolve(document, item);
            }
        }
        _ => {}
    }
    schema
}

fn assert_matches(schema: &Value, instance: &Value, what: &str) {
    let validator = jsonschema::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("{what}: schema did not build: {error}"));
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{what} does not match its schema:\n{}\ninstance {instance}",
        errors.join("\n")
    );
}

fn documented_statuses(document: &Value) -> Vec<(String, String, String)> {
    let mut statuses = Vec::new();
    let Some(paths) = document["paths"].as_object() else {
        return statuses;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for (method, operation) in item {
            let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
                continue;
            };
            for status in responses.keys() {
                statuses.push((method.clone(), path.clone(), status.clone()));
            }
        }
    }
    statuses
}
