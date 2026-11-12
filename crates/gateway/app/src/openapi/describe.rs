//! The document's info, security schemes, tags, and the loose-schema allowlist.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::operations::Operation;
use super::render::{at_mut, first_paragraph, loose_pointers};
use super::source;

pub(super) fn info() -> Result<Value, String> {
    let text = std::fs::read_to_string(source::root().join("crates/gateway/README.md"))
        .map_err(|error| format!("crates/gateway/README.md: {error}"))?;
    let description = text
        .lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(json!({
        "description": description,
        "license": { "identifier": "BSL-1.0", "name": "BSL-1.0" },
        "title": "Runvil Gateway",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

pub(super) fn security_schemes() -> Result<Value, String> {
    let auth = "crates/gateway/app/src/auth.rs";
    let bearer =
        source::doc(auth, "check_auth").map_err(|error| error.message(auth, "check_auth"))?;
    let cookie = "crates/gateway/app/src/auth/primitives.rs";
    let session =
        source::doc(cookie, "AUTH_COOKIE").map_err(|error| error.message(cookie, "AUTH_COOKIE"))?;
    Ok(json!({
        "bearer": {
            "description": first_paragraph(&bearer),
            "scheme": "bearer",
            "type": "http"
        },
        "sessionCookie": {
            "description": session,
            "in": "cookie",
            "name": "runvil-gateway-session",
            "type": "apiKey"
        }
    }))
}

pub(super) fn tags(operations: &[Operation]) -> Result<Vec<Value>, String> {
    let used: BTreeSet<&str> = operations.iter().map(|operation| operation.tag).collect();
    let mut tags = Vec::new();
    for (name, file) in tag_sources() {
        if !used.contains(name) {
            continue;
        }
        let doc = source::module_doc(file).map_err(|error| error.message(file, "module"))?;
        tags.push(json!({
            "description": first_paragraph(&doc),
            "name": name
        }));
    }
    Ok(tags)
}

/// Tag order, and the module file each tag's description is taken from.
fn tag_sources() -> &'static [(&'static str, &'static str)] {
    &[
        ("relay", "crates/gateway/app/src/relay.rs"),
        ("speech", "crates/gateway/app/src/speech.rs"),
        ("models", "crates/gateway/app/src/models.rs"),
        ("health", "crates/gateway/app/src/health.rs"),
        ("web_search", "crates/gateway/app/src/web_search.rs"),
        ("cache", "crates/gateway/app/src/cache.rs"),
        ("stt", "crates/gateway/stt/api/src/lib.rs"),
    ]
}

/// Pointers of schemas the code leaves open on purpose: a pass-through
/// `serde_json::Value` or a flattened `rest` map. Each one is a field the
/// gateway forwards without a schema of its own.
const LOOSE: &[&str] = &[
    "#/components/schemas/ChatChunk",
    "#/components/schemas/ChatChunkChoice",
    "#/components/schemas/ChatRequest",
    "#/components/schemas/ChatRequest/properties/messages/items",
    "#/components/schemas/ChatResponse",
    "#/components/schemas/ChatResponse/properties/choices/items",
    "#/components/schemas/EmbeddingRequest",
    "#/components/schemas/EmbeddingResponse",
    "#/components/schemas/EmbeddingResponse/properties/data/items",
    "#/components/schemas/RerankRequest",
    "#/components/schemas/RerankResponse",
    "#/components/schemas/RerankResponse/properties/results/items",
    "#/components/schemas/SpeechRequest",
];

/// Loose schemas that exist only in an `stt` build. A reduced build does not
/// generate them, so they are not stale entries there.
#[cfg(feature = "stt")]
const LOOSE_STT: &[&str] =
    &["#/components/schemas/VerboseJsonTranscription/properties/words/items"];

fn loose_allowlist() -> Vec<&'static str> {
    #[cfg(feature = "stt")]
    {
        let mut allowlist = LOOSE.to_vec();
        allowlist.extend(LOOSE_STT);
        allowlist
    }
    #[cfg(not(feature = "stt"))]
    {
        LOOSE.to_vec()
    }
}

pub(super) fn check_loose(document: &Value) -> Result<(), Vec<String>> {
    let found = loose_pointers(document);
    let allowlist = loose_allowlist();
    let mut errors = Vec::new();
    for pointer in &found {
        if !allowlist.contains(&pointer.as_str()) {
            errors.push(format!(
                "loose schema at {pointer} is not allowlisted; it needs a real schema or an entry in LOOSE"
            ));
        }
    }
    for pointer in &allowlist {
        if !found.iter().any(|found| found == pointer) {
            errors.push(format!(
                "allowlist entry {pointer} is not a loose schema; remove it from LOOSE"
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(super) fn mark_pass_through(document: &mut Value) {
    for pointer in loose_allowlist() {
        let Some(node) = at_mut(document, pointer) else {
            continue;
        };
        if node.as_bool() == Some(true) {
            *node = json!({ "x-runvil-pass-through": true });
        } else if let Some(object) = node.as_object_mut() {
            object.insert("x-runvil-pass-through".to_owned(), json!(true));
        }
    }
}

pub(super) fn check_refs(document: &Value) -> Result<(), Vec<String>> {
    let schemas = &document["components"]["schemas"];
    let mut referenced = BTreeSet::new();
    for name in super::refs_in(document) {
        referenced.insert(name);
    }
    if let Some(object) = document.as_object() {
        collect_event_names(object, &mut referenced);
    }
    let mut errors = Vec::new();
    if let Some(object) = schemas.as_object() {
        for name in object.keys() {
            if !referenced.contains(name) {
                errors.push(format!("unreferenced component schema `{name}`"));
            }
        }
    }
    for name in &referenced {
        if schemas.get(name).is_none() {
            errors.push(format!("$ref to missing component schema `{name}`"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_event_names(object: &Map<String, Value>, names: &mut BTreeSet<String>) {
    if let Some(Value::Array(events)) = object.get("x-runvil-event-schemas") {
        for event in events {
            if let Some(name) = event.as_str() {
                names.insert(name.to_owned());
            }
        }
    }
    for value in object.values() {
        match value {
            Value::Object(object) => collect_event_names(object, names),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(object) = item {
                        collect_event_names(object, names);
                    }
                }
            }
            _ => {}
        }
    }
}
