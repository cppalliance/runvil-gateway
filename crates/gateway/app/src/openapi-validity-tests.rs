//! The document against the OpenAPI 3.1 schema and the structural rules
//! the schema does not enforce.

use std::collections::BTreeSet;

use super::tests::generated;

const OAS_DIALECT: &str = "https://spec.openapis.org/oas/3.1/dialect/2024-11-10";
const OAS_META: &str = "https://spec.openapis.org/oas/3.1/meta/2024-11-10";
const OAS_SCHEMA: &str = "https://spec.openapis.org/oas/3.1/schema/2026-08-03";
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace",
];

fn oas(name: &str) -> serde_json::Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/oas-3.1")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn oas_registry() -> jsonschema::Registry<'static> {
    jsonschema::Registry::new()
        .add(OAS_DIALECT, oas("dialect-2024-11-10.json"))
        .and_then(|registry| registry.add(OAS_META, oas("meta-2024-11-10.json")))
        .and_then(|registry| registry.add(OAS_SCHEMA, oas("schema-2026-08-03.json")))
        .and_then(jsonschema::RegistryBuilder::prepare)
        .unwrap_or_else(|error| panic!("the OAS registry did not prepare offline: {error}"))
}

fn schema_base_validator(registry: &jsonschema::Registry<'static>) -> jsonschema::Validator {
    let schema = oas("schema-base-2026-08-03.json");
    jsonschema::options()
        .with_registry(registry)
        .build(&schema)
        .unwrap_or_else(|error| panic!("schema-base did not build: {error}"))
}

#[test]
fn the_document_is_openapi_3_1() {
    let document = generated();
    let registry = oas_registry();
    let validator = schema_base_validator(&registry);
    let errors: Vec<String> = validator
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
    let registry = oas_registry();
    let validator = schema_base_validator(&registry);
    assert!(
        !validator.is_valid(&document),
        "response code abc must be rejected"
    );
}

fn structural_errors(document: &serde_json::Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(paths) = document["paths"].as_object() else {
        return vec!["the document has no paths object".to_owned()];
    };
    let mut ids = BTreeSet::new();
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            errors.push(format!("{path} is not a path item"));
            continue;
        };
        for (method, operation) in item {
            if !HTTP_METHODS.contains(&method.as_str()) {
                errors.push(format!("{path} has a non-method field `{method}`"));
                continue;
            }
            let Some(operation) = operation.as_object() else {
                errors.push(format!("{method} {path} is not an operation"));
                continue;
            };
            match operation
                .get("responses")
                .and_then(serde_json::Value::as_object)
            {
                Some(responses) if !responses.is_empty() => {}
                _ => errors.push(format!("{method} {path} has no responses")),
            }
            match operation
                .get("operationId")
                .and_then(serde_json::Value::as_str)
            {
                Some(id) if ids.insert(id.to_owned()) => {}
                Some(id) => errors.push(format!("duplicate operationId `{id}`")),
                None => errors.push(format!("{method} {path} has no operationId")),
            }
            errors.extend(template_parameters(method, path, operation));
        }
    }
    errors.extend(reference_errors(document));
    errors
}

fn template_parameters(
    method: &str,
    path: &str,
    operation: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let mut templates = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return vec![format!("{method} {path} has an unclosed template")];
        };
        templates.push(rest[start + 1..start + end].to_owned());
        rest = &rest[start + end + 1..];
    }
    let declared: Vec<&str> = operation
        .get("parameters")
        .and_then(serde_json::Value::as_array)
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
        if !declared.contains(&name.as_str()) {
            errors.push(format!(
                "{method} {path} template `{{{name}}}` has no path parameter"
            ));
        }
    }
    for name in declared {
        if !templates.iter().any(|template| template == name) {
            errors.push(format!(
                "{method} {path} path parameter `{name}` is not in the template"
            ));
        }
    }
    errors
}

fn reference_errors(document: &serde_json::Value) -> Vec<String> {
    let mut refs = Vec::new();
    collect_refs("", document, &mut refs);
    let mut errors = Vec::new();
    let mut referenced = BTreeSet::new();
    for (pointer, reference) in &refs {
        if let Some(name) = reference.strip_prefix("#/components/schemas/") {
            if name.contains('/') {
                errors.push(format!("{pointer} names a nested schema `{reference}`"));
            }
            referenced.insert(name.to_owned());
        }
        if !pointer_exists(document, reference) {
            errors.push(format!("{pointer} $ref `{reference}` does not resolve"));
        }
    }
    collect_event_names(document, &mut referenced);
    if let Some(schemas) = document["components"]["schemas"].as_object() {
        for name in schemas.keys() {
            if !referenced.contains(name) {
                errors.push(format!("unreferenced component schema `{name}`"));
            }
        }
    }
    errors
}

fn collect_refs(pointer: &str, value: &serde_json::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                out.push((pointer.to_owned(), reference.to_owned()));
            }
            for (key, child) in object {
                collect_refs(&format!("{pointer}/{key}"), child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_refs(&format!("{pointer}/{index}"), child, out);
            }
        }
        _ => {}
    }
}

fn collect_event_names(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::Array(events)) = object.get("x-runvil-event-schemas") {
                for event in events {
                    if let Some(name) = event.as_str() {
                        names.insert(name.to_owned());
                    }
                }
            }
            for child in object.values() {
                collect_event_names(child, names);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_event_names(item, names);
            }
        }
        _ => {}
    }
}

fn pointer_exists(document: &serde_json::Value, reference: &str) -> bool {
    let Some(path) = reference.strip_prefix("#/") else {
        return false;
    };
    let mut cursor = document;
    for segment in path.split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        cursor = match cursor {
            serde_json::Value::Object(object) => match object.get(&segment) {
                Some(child) => child,
                None => return false,
            },
            serde_json::Value::Array(items) => {
                match segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| items.get(index))
                {
                    Some(child) => child,
                    None => return false,
                }
            }
            _ => return false,
        };
    }
    true
}
