//! The committed OpenAPI document, read so a test can check what the running
//! gateway answered against what the document lists.

use serde_json::{Map, Value};

/// The `responses` the committed `openapi.json` lists for `method` on `path`.
///
/// # Panics
///
/// When the file is unreadable, is not JSON, or does not document the
/// operation, naming the operation.
pub(crate) fn documented_responses(path: &str, method: &str) -> Map<String, Value> {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    let document: Value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    document["paths"][path][method]["responses"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("openapi.json documents no {method} {path}"))
}
