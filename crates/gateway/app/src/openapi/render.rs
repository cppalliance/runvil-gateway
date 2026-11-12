//! Canonical JSON and the drift report.
//!
//! Object keys are sorted, so the artifact does not depend on the order a
//! map was built in. `serde_json` is banned from `preserve_order`
//! (`deny.toml`), which is the same ordering.

use serde_json::{Map, Value};

/// `value` with every object's keys sorted.
pub(super) fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&object[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        scalar => scalar.clone(),
    }
}

/// Two-space indent and a trailing newline, the bytes the drift test compares.
pub(super) fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&canonical(value))
        .unwrap_or_else(|error| panic!("the document must serialize: {error}"));
    text.push('\n');
    text
}

/// JSON pointers that differ, at most `limit`, then how many were left out.
pub(super) fn diff(committed: &Value, generated: &Value, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    differ("", committed, generated, &mut out);
    if out.len() > limit {
        let hidden = out.len() - limit;
        out.truncate(limit);
        out.push(format!("{hidden} further differences not shown"));
    }
    out
}

fn differ(pointer: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    if left == right {
        return;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = format!("{pointer}/{key}");
                match (left.get(key), right.get(key)) {
                    (Some(left), Some(right)) => differ(&child, left, right, out),
                    (Some(_), None) => out.push(format!("{child} removed")),
                    (None, Some(_)) => out.push(format!("{child} added")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                differ(&format!("{pointer}/{index}"), left, right, out);
            }
        }
        _ => out.push(format!(
            "{pointer} changed from {} to {}",
            compact(left),
            compact(right)
        )),
    }
}

fn compact(value: &Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_owned());
    if text.len() > 180 {
        format!("{}…", &text[..180])
    } else {
        text
    }
}

/// Insert `value` at `pointer` when the pointer addresses an existing node.
pub(super) fn at_mut<'a>(root: &'a mut Value, pointer: &str) -> Option<&'a mut Value> {
    let mut cursor = root;
    for segment in pointer
        .trim_start_matches('#')
        .trim_start_matches('/')
        .split('/')
    {
        if segment.is_empty() {
            continue;
        }
        let segment = segment.replace("~1", "/").replace("~0", "~");
        cursor = match cursor {
            Value::Object(object) => object.get_mut(&segment)?,
            Value::Array(items) => items.get_mut(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cursor)
}

/// Every pointer whose schema is `true`, `{}`, or `"additionalProperties": true`.
///
/// A boolean `true` counts only where JSON Schema puts a schema: `schema`,
/// `items`, a `properties` value, an `anyOf` branch. `required: true` and the
/// extension flags are keywords, not schemas.
pub(super) fn loose_pointers(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_loose("", value, &mut out, false);
    out
}

fn walk_loose(pointer: &str, value: &Value, out: &mut Vec<String>, is_schema: bool) {
    match value {
        Value::Bool(true) if is_schema => out.push(format!("#{pointer}")),
        Value::Object(object) if object.is_empty() && is_schema => out.push(format!("#{pointer}")),
        Value::Object(object) => {
            let open = is_schema
                && object.get("additionalProperties").and_then(Value::as_bool) == Some(true);
            if open {
                out.push(format!("#{pointer}"));
            }
            for (key, child) in object {
                if open && key == "additionalProperties" {
                    continue;
                }
                let child_pointer = format!("{pointer}/{}", escape_pointer(key));
                match schema_child(key, is_schema, pointer) {
                    SchemaChild::Schema => walk_loose(&child_pointer, child, out, true),
                    SchemaChild::Map => walk_schema_map(&child_pointer, child, out),
                    SchemaChild::List => walk_schema_list(&child_pointer, child, out),
                    SchemaChild::Other => walk_loose(&child_pointer, child, out, false),
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_loose(&format!("{pointer}/{index}"), child, out, false);
            }
        }
        _ => {}
    }
}

enum SchemaChild {
    Schema,
    Map,
    List,
    Other,
}

fn schema_child(key: &str, parent_is_schema: bool, pointer: &str) -> SchemaChild {
    if matches!(
        key,
        "schema"
            | "items"
            | "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "contains"
            | "propertyNames"
            | "if"
            | "then"
            | "else"
            | "not"
            | "contentSchema"
    ) {
        return SchemaChild::Schema;
    }
    if key == "schemas" && (pointer == "/components" || pointer.ends_with("/components")) {
        return SchemaChild::Map;
    }
    if parent_is_schema
        && matches!(
            key,
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
        )
    {
        return SchemaChild::Map;
    }
    if parent_is_schema && matches!(key, "allOf" | "anyOf" | "oneOf" | "prefixItems") {
        return SchemaChild::List;
    }
    SchemaChild::Other
}

fn walk_schema_map(pointer: &str, value: &Value, out: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, child) in object {
        walk_loose(
            &format!("{pointer}/{}", escape_pointer(key)),
            child,
            out,
            true,
        );
    }
}

fn walk_schema_list(pointer: &str, value: &Value, out: &mut Vec<String>) {
    let Some(items) = value.as_array() else {
        return;
    };
    for (index, child) in items.iter().enumerate() {
        walk_loose(&format!("{pointer}/{index}"), child, out, true);
    }
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The first paragraph of a doc comment: up to the first blank line.
pub(super) fn first_paragraph(doc: &str) -> String {
    doc.lines()
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An empty object, named so call sites do not repeat the construction.
pub(super) fn object() -> Map<String, Value> {
    Map::new()
}
