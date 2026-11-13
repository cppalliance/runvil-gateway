//! Two responses that share a status, merged into one entry without losing
//! either one's description, sources, error codes, or body.

use serde_json::{Map, Value};

#[cfg(test)]
#[path = "merge-tests.rs"]
mod tests;

/// The fields whose values are lists: a merge keeps every distinct item.
const LISTS: &[&str] = &[
    "x-runvil-error-codes",
    "x-runvil-error-types",
    "x-runvil-source",
];

/// Inserts `value` under `key`, or merges it into the response already there.
///
/// Descriptions are joined by a blank line, in merge order. The sources and
/// the error codes and types keep every distinct item; a source stays a
/// single string until a second, different one joins it. Any other field,
/// such as `content` or `headers`, must be absent from one side or equal on
/// both, because one status cannot carry two different bodies.
pub(super) fn merge_response(
    responses: &mut Map<String, Value>,
    key: String,
    value: Value,
) -> Result<(), String> {
    let Some(existing) = responses.get_mut(&key) else {
        responses.insert(key, value);
        return Ok(());
    };
    let (Some(existing), Value::Object(incoming)) = (existing.as_object_mut(), value) else {
        return Err(format!("ResponseConflict: {key} is not a response object"));
    };
    for (field, right) in incoming {
        let Some(left) = existing.get_mut(&field) else {
            existing.insert(field, right);
            continue;
        };
        if field == "description" {
            let joined = format!(
                "{}\n\n{}",
                left.as_str().unwrap_or(""),
                right.as_str().unwrap_or("")
            );
            *left = Value::String(joined);
        } else if LISTS.contains(&field.as_str()) {
            *left = union(&field, left, &right);
        } else if *left != right {
            return Err(format!(
                "ResponseConflict: {key} has two different `{field}` values"
            ));
        }
    }
    Ok(())
}

/// The distinct items of both sides. Error codes and types stay sorted, as
/// `errors::responses` writes them; sources keep merge order.
fn union(field: &str, left: &Value, right: &Value) -> Value {
    let mut merged = items(left);
    for item in items(right) {
        if !merged.contains(&item) {
            merged.push(item);
        }
    }
    if field == "x-runvil-source" {
        if merged.len() == 1 {
            return merged.remove(0);
        }
    } else {
        merged.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    }
    Value::Array(merged)
}

/// A list field's items: the array's elements, or the one bare value.
fn items(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        single => vec![single.clone()],
    }
}
