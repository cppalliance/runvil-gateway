//! The description against the official OpenAPI 3.1 schema, offline, and
//! the structural rules that schema does not enforce.

use std::collections::BTreeSet;

use serde_json::Value;

use super::tests::{generated, named_components, operations};

const OAS_DIALECT: &str = "https://spec.openapis.org/oas/3.1/dialect/2024-11-10";
const OAS_META: &str = "https://spec.openapis.org/oas/3.1/meta/2024-11-10";
const OAS_SCHEMA: &str = "https://spec.openapis.org/oas/3.1/schema/2026-08-03";

/// Keywords that constrain a schema. A schema with none of them accepts
/// any value, whatever else it says.
const CONSTRAINING: &[&str] = &[
    "type",
    "$ref",
    "enum",
    "const",
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "properties",
    "items",
    "prefixItems",
    "additionalProperties",
    "required",
    "format",
    "pattern",
];

fn oas(name: &str) -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/oas-3.1")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn schema_base_validator() -> jsonschema::Validator {
    let registry = jsonschema::Registry::new()
        .add(OAS_DIALECT, oas("dialect-2024-11-10.json"))
        .and_then(|registry| registry.add(OAS_META, oas("meta-2024-11-10.json")))
        .and_then(|registry| registry.add(OAS_SCHEMA, oas("schema-2026-08-03.json")))
        .and_then(jsonschema::RegistryBuilder::prepare)
        .unwrap_or_else(|error| panic!("the OAS registry did not prepare offline: {error}"));
    jsonschema::options()
        .with_registry(&registry)
        .build(&oas("schema-base-2026-08-03.json"))
        .unwrap_or_else(|error| panic!("schema-base did not build: {error}"))
}

/// The document validates against `schema-base`, which also checks every
/// schema object against the OAS dialect, and keeps the structural rules.
#[test]
fn the_document_is_openapi_3_1() {
    let document = generated();
    let errors: Vec<String> = schema_base_validator()
        .iter_errors(&document)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "the document is not OpenAPI 3.1:\n{}",
        errors.join("\n")
    );
    let structural = structural_errors(&document);
    assert!(
        structural.is_empty(),
        "the document breaks a structural rule:\n{}",
        structural.join("\n")
    );
}

/// The validator rejects what it should, so a pass means something.
#[test]
fn a_bad_response_code_is_rejected() {
    let mut document = generated();
    let responses = document["paths"]["/health"]["get"]["responses"]
        .as_object_mut()
        .expect("health responses");
    responses.insert(
        "abc".to_owned(),
        serde_json::json!({ "description": "not a status" }),
    );
    assert!(
        !schema_base_validator().is_valid(&document),
        "response code abc must be rejected"
    );
}

/// A validator for one component of the generated document, resolving its
/// `$ref`s inside the document.
fn component_validator(document: &Value, name: &str) -> jsonschema::Validator {
    const DOCUMENT: &str = "urn:runvil:gateway-openapi";
    let registry = jsonschema::Registry::new()
        .add(DOCUMENT, document.clone())
        .and_then(jsonschema::RegistryBuilder::prepare)
        .unwrap_or_else(|error| panic!("the document did not load as a resource: {error}"));
    jsonschema::options()
        .with_registry(&registry)
        .build(&serde_json::json!({ "$ref": format!("{DOCUMENT}#/components/schemas/{name}") }))
        .unwrap_or_else(|error| panic!("{name} did not build: {error}"))
}

/// The union replies whose branches overlap accept a body of each branch.
/// utoipa writes them as `oneOf`, which rejects a body that matches two
/// branches; the markers widen them to `anyOf`.
#[test]
fn each_branch_of_an_overlapping_union_validates() {
    let document = generated();
    let model = serde_json::to_value(crate::wire::ModelInfo {
        id: "chat-model".to_owned(),
        object: "model",
        kind: gateway_api_types::ModelKind::Chat,
        description: "A chat model.".to_owned(),
        context: 8192,
        thinking: gateway_api_types::ThinkingMode::Never,
        capabilities: gateway_api_types::Capabilities::default(),
    })
    .expect("a model serializes");
    let mut bodies = vec![("CatalogModelInfo", model)];
    if cfg!(feature = "stt") {
        bodies.push((
            "CatalogModelInfo",
            serde_json::json!({ "id": "speech-model", "object": "model", "kind": "transcription" }),
        ));
        bodies.push(("TranscriptionResponse", serde_json::json!({ "text": "hi" })));
        bodies.push((
            "TranscriptionResponse",
            serde_json::json!({
                "task": "transcribe",
                "language": "en",
                "duration": 1.5,
                "text": "hi",
                "segments": [{ "id": 0, "start": 0.0, "end": 1.5, "text": "hi" }],
                "words": []
            }),
        ));
    }
    for (name, body) in bodies {
        let validator = component_validator(&document, name);
        let errors: Vec<String> = validator
            .iter_errors(&body)
            .map(|error| error.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "{name} rejects {body}:\n{}",
            errors.join("\n")
        );
    }
}

/// Every schema that accepts any value is marked `x-runvil-pass-through`,
/// including one that carries only a description.
#[test]
fn every_open_schema_is_marked_pass_through() {
    let document = generated();
    let mut open = Vec::new();
    open_schemas(
        "/components/schemas",
        &document["components"]["schemas"],
        true,
        &mut open,
    );
    open_schemas("/paths", &document["paths"], false, &mut open);
    assert!(
        open.is_empty(),
        "open schemas without x-runvil-pass-through:\n{}",
        open.join("\n")
    );
}

/// Walks `value`. `schemas` says whether `value` is a map of schemas;
/// outside one, the walk looks for `schema` keys.
fn open_schemas(pointer: &str, value: &Value, schemas: bool, out: &mut Vec<String>) {
    let Value::Object(object) = value else {
        return;
    };
    for (key, child) in object {
        let child_pointer = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
        if schemas || key == "schema" {
            schema(&child_pointer, child, out);
        } else {
            open_schemas(&child_pointer, child, false, out);
        }
    }
}

/// Checks one schema and every schema inside it.
fn schema(pointer: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Bool(true) => out.push(pointer.to_owned()),
        Value::Object(object) => {
            let constrained = object
                .keys()
                .any(|key| CONSTRAINING.contains(&key.as_str()));
            let marked = object.get("x-runvil-pass-through") == Some(&Value::Bool(true));
            if !constrained && !marked {
                out.push(pointer.to_owned());
            }
            for key in ["items", "additionalProperties", "not"] {
                if let Some(child) = object.get(key) {
                    schema(&format!("{pointer}/{key}"), child, out);
                }
            }
            if let Some(Value::Object(children)) = object.get("properties") {
                for (name, child) in children {
                    schema(&format!("{pointer}/properties/{name}"), child, out);
                }
            }
            for key in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(Value::Array(children)) = object.get(key) {
                    for (index, child) in children.iter().enumerate() {
                        schema(&format!("{pointer}/{key}/{index}"), child, out);
                    }
                }
            }
        }
        _ => {}
    }
}

fn structural_errors(document: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let mut ids = BTreeSet::new();
    for (path, method, operation) in operations(document) {
        if operation["responses"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
        {
            errors.push(format!("{method} {path} has no responses"));
        }
        match operation["operationId"].as_str() {
            Some(id) if ids.insert(id.to_owned()) => {}
            Some(id) => errors.push(format!("duplicate operationId `{id}`")),
            None => errors.push(format!("{method} {path} has no operationId")),
        }
        errors.extend(template_parameters(&method, &path, &operation));
    }
    errors.extend(reference_errors(document));
    errors
}

fn template_parameters(method: &str, path: &str, operation: &Value) -> Vec<String> {
    let templates: Vec<&str> = path
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .collect();
    let declared: Vec<&str> = operation["parameters"]
        .as_array()
        .map(|parameters| {
            parameters
                .iter()
                .filter(|parameter| parameter["in"] == "path")
                .filter_map(|parameter| parameter["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut errors = Vec::new();
    for name in &templates {
        if !declared.contains(name) {
            errors.push(format!(
                "{method} {path} template `{{{name}}}` has no path parameter"
            ));
        }
    }
    for name in &declared {
        if !templates.contains(name) {
            errors.push(format!(
                "{method} {path} path parameter `{name}` is not in the template"
            ));
        }
    }
    errors
}

/// Every `$ref` resolves, and every component is named by a `$ref` or an
/// event schema marker.
fn reference_errors(document: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let mut refs = Vec::new();
    collect_refs(document, &mut refs);
    for reference in refs {
        let resolves = reference
            .strip_prefix('#')
            .and_then(|pointer| document.pointer(pointer))
            .is_some();
        if !resolves {
            errors.push(format!("$ref `{reference}` does not resolve"));
        }
    }
    let mut named = BTreeSet::new();
    named_components(document, &mut named);
    if let Some(schemas) = document["components"]["schemas"].as_object() {
        for name in schemas.keys() {
            if !named.contains(name) {
                errors.push(format!("unreferenced component schema `{name}`"));
            }
        }
    }
    errors
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                out.push(reference.to_owned());
            }
            for child in object.values() {
                collect_refs(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}
