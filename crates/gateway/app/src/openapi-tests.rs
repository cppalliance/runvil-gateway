//! The committed description against this build, and against the route
//! registry.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::http::Method;
use serde_json::Value;

use super::{document, missing_features, render};
use crate::registry::{self, Tier};

/// The one command that rewrites `openapi.json`.
const REGENERATE: &str = "BLESS_OPENAPI=1 cargo test -p gateway --lib openapi";

/// The methods an OpenAPI path item can hold.
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace",
];

/// Open-tier routes the description leaves out, with the reason.
const EXCLUDED: &[(&str, Method, &str)] = &[
    ("/admin/profiles", Method::GET, OPEN_ADMIN),
    ("/admin/switch-profile", Method::POST, OPEN_ADMIN),
    ("/admin/status", Method::GET, OPEN_ADMIN),
    ("/admin/progress", Method::GET, OPEN_ADMIN),
    ("/admin/queue/cancel", Method::POST, OPEN_ADMIN),
    ("/admin/queue/cancel-pending", Method::POST, OPEN_ADMIN),
];

/// Why the open admin tier is left out.
const OPEN_ADMIN: &str = "the open admin tier serves the settings page and the tray, and documenting it is a separate decision";

/// Why the walled tier is left out, every route of it: it is loopback-only,
/// it reads secrets in plaintext, writes files, or launches processes, and
/// its one client is the embedded settings page.
const WALLED: &str = "the walled tier is the settings page's private surface";

/// The generated description, or a panic naming every marker that failed.
pub(super) fn generated() -> Value {
    document()
        .unwrap_or_else(|errors| panic!("the description did not generate:\n{}", errors.join("\n")))
}

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

/// The committed file equals a fresh generation in a build with every
/// route feature. A build missing some checks the equation that makes the
/// `x-runvil-feature` markers honest: the committed document, with every
/// element marked for a missing feature removed, is what this build
/// generates.
#[test]
fn the_committed_document_matches_this_build() {
    let generated = generated();
    let missing = missing_features();
    if std::env::var_os("BLESS_OPENAPI").is_some() {
        assert!(
            missing.is_empty(),
            "blessing needs every route feature; this build lacks {missing:?}"
        );
        let text = render(&generated).expect("the description renders");
        std::fs::write(committed_path(), text).expect("openapi.json is writable");
        return;
    }
    let text = std::fs::read_to_string(committed_path())
        .unwrap_or_else(|error| panic!("openapi.json is unreadable ({error}); run {REGENERATE}"));
    let committed: Value = serde_json::from_str(&text).expect("openapi.json is JSON");
    if missing.is_empty() {
        let fresh = render(&generated).expect("the description renders");
        let changed = differences("", &committed, &generated);
        assert!(
            text == fresh,
            "openapi.json is stale at {} pointers:\n{}\nrun {REGENERATE}",
            changed.len(),
            changed
                .iter()
                .take(50)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    } else {
        let expected = without_features(committed, &missing);
        let changed = differences("", &expected, &generated);
        assert!(
            changed.is_empty(),
            "without {missing:?}, the committed description and this build differ at:\n{}",
            changed
                .iter()
                .take(50)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// The JSON pointers at which two values differ.
fn differences(pointer: &str, left: &Value, right: &Value) -> Vec<String> {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let keys: BTreeSet<&String> = left.keys().chain(right.keys()).collect();
            keys.into_iter()
                .flat_map(|key| {
                    let child = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                    match (left.get(key), right.get(key)) {
                        (Some(left), Some(right)) => differences(&child, left, right),
                        (Some(_), None) => vec![format!("removed {child}")],
                        _ => vec![format!("added {child}")],
                    }
                })
                .collect()
        }
        _ if left == right => Vec::new(),
        _ => vec![format!("changed {pointer}")],
    }
}

/// The committed description as a build without `missing` writes it:
/// operations and components marked with a missing feature removed, the
/// `$ref` branches that named a removed component dropped, and every
/// component no remaining operation reaches removed.
fn without_features(mut document: Value, missing: &[&str]) -> Value {
    let marked = |value: &Value| {
        value
            .get("x-runvil-feature")
            .and_then(Value::as_str)
            .is_some_and(|feature| missing.contains(&feature))
    };
    if let Some(paths) = document["paths"].as_object_mut() {
        for item in paths.values_mut() {
            if let Some(item) = item.as_object_mut() {
                item.retain(|_, operation| !marked(operation));
            }
        }
        paths.retain(|_, item| item.as_object().is_some_and(|item| !item.is_empty()));
    }
    let mut removed = BTreeSet::new();
    if let Some(schemas) = document["components"]["schemas"].as_object_mut() {
        schemas.retain(|name, schema| {
            let keep = !marked(schema);
            if !keep {
                removed.insert(format!("#/components/schemas/{name}"));
            }
            keep
        });
    }
    drop_branches(&mut document, &removed);
    let reached = reachable(&document);
    if let Some(schemas) = document["components"]["schemas"].as_object_mut() {
        schemas.retain(|name, _| reached.contains(name));
    }
    document
}

/// Removes every `oneOf` or `anyOf` branch that is a `$ref` to a removed
/// component.
fn drop_branches(value: &mut Value, removed: &BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for key in ["oneOf", "anyOf"] {
                if let Some(Value::Array(branches)) = object.get_mut(key) {
                    branches.retain(|branch| {
                        branch
                            .get("$ref")
                            .and_then(Value::as_str)
                            .is_none_or(|target| !removed.contains(target))
                    });
                }
            }
            for child in object.values_mut() {
                drop_branches(child, removed);
            }
        }
        Value::Array(items) => {
            for item in items {
                drop_branches(item, removed);
            }
        }
        _ => {}
    }
}

/// The component names the operations reach through `$ref` and the event
/// schema markers, followed through the components themselves.
fn reachable(document: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    named_components(&document["paths"], &mut names);
    let mut pending: Vec<String> = names.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        let mut found = BTreeSet::new();
        named_components(&document["components"]["schemas"][&name], &mut found);
        for name in found {
            if names.insert(name.clone()) {
                pending.push(name);
            }
        }
    }
    names
}

/// Collects every component a value names, by `$ref` or by an
/// `x-runvil-event-schemas` marker.
pub(super) fn named_components(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|target| target.strip_prefix("#/components/schemas/"))
            {
                names.insert(name.to_owned());
            }
            if let Some(Value::Array(events)) = object.get("x-runvil-event-schemas") {
                names.extend(events.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for child in object.values() {
                named_components(child, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                named_components(item, names);
            }
        }
        _ => {}
    }
}

/// Every operation in the document, as `(path, method, operation)`.
pub(super) fn operations(document: &Value) -> Vec<(String, String, Value)> {
    let mut found = Vec::new();
    if let Some(paths) = document["paths"].as_object() {
        for (path, item) in paths {
            for method in HTTP_METHODS {
                if let Some(operation) = item.get(*method) {
                    found.push((path.clone(), (*method).to_owned(), operation.clone()));
                }
            }
        }
    }
    found
}

/// Every `(path, method)` the router mounts is documented or excluded with
/// a reason, every documented one is mounted, and no exclusion is stale.
#[test]
fn every_mounted_route_is_documented_or_excluded() {
    let documented: BTreeSet<(String, String)> = operations(&generated())
        .into_iter()
        .map(|(path, method, _)| (path, method.to_uppercase()))
        .collect();
    let mut mounted = BTreeSet::new();
    let mut problems = Vec::new();
    for route in registry::all() {
        for method in route.methods {
            let key = (route.path.to_owned(), method.as_str().to_owned());
            mounted.insert(key.clone());
            let reason = if route.tier == Tier::Walled {
                Some(WALLED)
            } else {
                EXCLUDED
                    .iter()
                    .find(|(path, excluded, _)| *path == route.path && excluded == method)
                    .map(|(_, _, reason)| *reason)
            };
            match (documented.contains(&key), reason) {
                (true, Some(reason)) => problems.push(format!(
                    "{method} {} is documented and excluded: {reason}",
                    route.path
                )),
                (false, None) => problems.push(format!(
                    "{method} {} is mounted but neither documented nor excluded",
                    route.path
                )),
                _ => {}
            }
        }
    }
    for (path, method) in &documented {
        if !mounted.contains(&(path.clone(), method.clone())) {
            problems.push(format!("{method} {path} is documented but not mounted"));
        }
    }
    for (path, method, _) in EXCLUDED {
        if !mounted.contains(&((*path).to_owned(), method.as_str().to_owned())) {
            problems.push(format!(
                "the exclusion of {method} {path} names no mounted route"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Each operation carries the feature its route is compiled under, read
/// from the registry: the cache routes are `local`, the web search route
/// is `web-search`, and the speech crate's routes are `stt`. This is the
/// check of the `local` markers, which no test build can turn off.
#[test]
fn each_operation_carries_the_feature_that_mounts_it() {
    let mut problems = Vec::new();
    for (path, method, operation) in operations(&generated()) {
        let method = method.to_uppercase();
        let in_routes = |routes: &[registry::RouteInfo]| {
            routes.iter().any(|route| {
                route.path == path && route.methods.iter().any(|m| m.as_str() == method)
            })
        };
        let expected = if in_routes(crate::cache::ROUTES) {
            Some("local")
        } else if web_search_route(&in_routes) {
            Some("web-search")
        } else if speech_route(&path) {
            Some("stt")
        } else {
            None
        };
        let marked = operation.get("x-runvil-feature").and_then(Value::as_str);
        if marked != expected {
            problems.push(format!(
                "{method} {path} is marked {marked:?}, mounted under {expected:?}"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[cfg(feature = "web-search")]
fn web_search_route(in_routes: &dyn Fn(&[registry::RouteInfo]) -> bool) -> bool {
    in_routes(crate::web_search::ROUTES)
}

#[cfg(not(feature = "web-search"))]
fn web_search_route(_: &dyn Fn(&[registry::RouteInfo]) -> bool) -> bool {
    false
}

#[cfg(feature = "stt")]
fn speech_route(path: &str) -> bool {
    path == gateway_stt::TRANSCRIPTIONS_PATH || path == gateway_stt::REALTIME_PATH
}

#[cfg(not(feature = "stt"))]
fn speech_route(_: &str) -> bool {
    false
}

/// Every operation is classified as following a standard or as an
/// extension, never both; every authenticated one marks loopback trust;
/// every error response with a JSON body lists its error codes.
#[test]
fn every_operation_carries_its_conventions() {
    let mut problems = Vec::new();
    for (path, method, operation) in operations(&generated()) {
        let standard = operation.get("x-runvil-standard").is_some();
        let extension = operation.get("x-runvil-extension") == Some(&Value::Bool(true));
        if standard == extension {
            problems.push(format!(
                "{method} {path} is not exactly one of standard or extension"
            ));
        }
        if extension && operation.get("x-runvil-opt-in").is_none() {
            problems.push(format!(
                "{method} {path} is an extension with no x-runvil-opt-in"
            ));
        }
        let anonymous = operation["security"] == serde_json::json!([{}]);
        let trusts = operation.get("x-runvil-loopback-trust") == Some(&Value::Bool(true));
        if anonymous == trusts {
            problems.push(format!(
                "{method} {path}: anonymous {anonymous}, x-runvil-loopback-trust {trusts}"
            ));
        }
        let Some(responses) = operation["responses"].as_object() else {
            problems.push(format!("{method} {path} has no responses"));
            continue;
        };
        for (status, response) in responses {
            let error = status.starts_with('4') || status.starts_with('5');
            let json = response.pointer("/content/application~1json").is_some();
            let codes = response.get("x-runvil-error-codes").is_some();
            if error && json != codes {
                problems.push(format!(
                    "{method} {path} {status}: JSON body {json}, error codes {codes}"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
