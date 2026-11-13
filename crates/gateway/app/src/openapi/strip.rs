//! Removes the elements a build without `web-search`, `stt`, and `config-ui`
//! does not compile, so that build's document can be compared with this one.
//! `local` stays: no test build can turn it off.

use serde_json::{Map, Value};

const REDUCED: &[&str] = &["web-search", "stt", "config-ui"];

const METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace",
];

/// The document a reduced build generates from the same types.
pub(super) fn strip_reduced(document: &Value) -> Value {
    let mut document = document.clone();
    strip_operations(&mut document);
    strip_marked_schemas(&mut document);
    strip_branches(&mut document);
    strip_unused_tags(&mut document);
    drop_unreferenced(&mut document);
    document
}

fn marked(value: &Value) -> bool {
    value
        .get("x-runvil-feature")
        .and_then(Value::as_str)
        .is_some_and(|feature| REDUCED.contains(&feature))
}

fn strip_operations(document: &mut Value) {
    let Some(paths) = document.get_mut("paths").and_then(Value::as_object_mut) else {
        return;
    };
    let names: Vec<String> = paths.keys().cloned().collect();
    for name in names {
        let Some(item) = paths.get_mut(&name).and_then(Value::as_object_mut) else {
            continue;
        };
        let methods: Vec<String> = item
            .keys()
            .filter(|key| METHODS.contains(&key.as_str()))
            .filter(|key| item.get(*key).is_some_and(marked))
            .cloned()
            .collect();
        for method in methods {
            item.remove(&method);
        }
        let still = item.keys().any(|key| METHODS.contains(&key.as_str()));
        if !still {
            paths.remove(&name);
        }
    }
}

fn strip_marked_schemas(document: &mut Value) {
    let Some(schemas) = schemas_mut(document) else {
        return;
    };
    let names: Vec<String> = schemas
        .iter()
        .filter(|(_, schema)| marked(schema))
        .map(|(name, _)| name.clone())
        .collect();
    for name in names {
        schemas.remove(&name);
    }
}

fn strip_branches(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["anyOf", "oneOf"] {
                let Some(Value::Array(branches)) = map.get_mut(key) else {
                    continue;
                };
                branches.retain(|branch| !marked(branch));
            }
            for child in map.values_mut() {
                strip_branches(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_branches(item);
            }
        }
        _ => {}
    }
}

fn strip_unused_tags(document: &mut Value) {
    let mut used = Vec::new();
    if let Some(paths) = document.get("paths").and_then(Value::as_object) {
        for item in paths.values() {
            let Some(item) = item.as_object() else {
                continue;
            };
            for (method, operation) in item {
                if !METHODS.contains(&method.as_str()) {
                    continue;
                }
                let Some(tags) = operation.get("tags").and_then(Value::as_array) else {
                    continue;
                };
                for tag in tags {
                    if let Some(tag) = tag.as_str()
                        && !used.contains(&tag.to_owned())
                    {
                        used.push(tag.to_owned());
                    }
                }
            }
        }
    }
    let Some(tags) = document.get_mut("tags").and_then(Value::as_array_mut) else {
        return;
    };
    tags.retain(|tag| {
        tag.get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| used.iter().any(|used| used == name))
    });
}

fn drop_unreferenced(document: &mut Value) {
    loop {
        let mut referenced = std::collections::BTreeSet::new();
        collect_refs(document, &mut referenced);
        collect_events(document, &mut referenced);
        let Some(schemas) = schemas_mut(document) else {
            return;
        };
        let stale: Vec<String> = schemas
            .keys()
            .filter(|name| !referenced.contains(*name))
            .cloned()
            .collect();
        if stale.is_empty() {
            return;
        }
        for name in stale {
            schemas.remove(&name);
        }
    }
}

fn collect_refs(value: &Value, names: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str)
                && let Some(name) = reference.strip_prefix("#/components/schemas/")
            {
                names.insert(name.to_owned());
            }
            for child in map.values() {
                collect_refs(child, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, names);
            }
        }
        _ => {}
    }
}

fn collect_events(value: &Value, names: &mut std::collections::BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(events)) = map.get("x-runvil-event-schemas") {
                for event in events {
                    if let Some(name) = event.as_str() {
                        names.insert(name.to_owned());
                    }
                }
            }
            for child in map.values() {
                collect_events(child, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_events(item, names);
            }
        }
        _ => {}
    }
}

fn schemas_mut(document: &mut Value) -> Option<&mut Map<String, Value>> {
    document
        .get_mut("components")
        .and_then(|components| components.get_mut("schemas"))
        .and_then(Value::as_object_mut)
}
