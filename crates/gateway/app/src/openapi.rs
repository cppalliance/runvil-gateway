//! The generated OpenAPI document. Test-only: nothing here is in the binary.
//!
//! [`document`] fails with every problem it found, each one naming the route,
//! type, or pointer. It does not skip a route or substitute an empty schema.

mod errors;
mod fields;
mod merge;
mod operations;
mod render;
mod schemas;
mod source;
mod strip;
mod success;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

use self::merge::merge_response;
use self::operations::{Content, Contract, Operation};
use self::render::object;

type Usage = BTreeMap<String, BTreeSet<Option<&'static str>>>;

#[cfg(test)]
#[path = "openapi-tests.rs"]
mod tests;

#[cfg(test)]
#[path = "openapi-validity-tests.rs"]
mod validity_tests;

#[cfg(test)]
#[path = "openapi-conformance-tests.rs"]
mod conformance_tests;

#[cfg(test)]
#[path = "openapi-audit-tests.rs"]
mod audit_tests;

mod describe;
mod exclusions;

/// The document, or every problem found while building it.
pub(super) fn document() -> Result<Value, Vec<String>> {
    let mut errors = Vec::new();
    let operations = operations::operations().map_err(|error| vec![error])?;
    let mut request = generator(Contract::Request);
    let mut response = generator(Contract::Response);
    let mut usage = Usage::new();
    let mut paths = object();
    for operation in &operations {
        match build_operation(operation, &mut request, &mut response, &mut usage) {
            Ok((path, method, value)) => {
                let entry = paths.entry(path).or_insert_with(|| Value::Object(object()));
                entry
                    .as_object_mut()
                    .expect("a path item")
                    .insert(method, value);
            }
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut schemas = merge_definitions(&mut request, &mut response).unwrap_or_else(|error| {
        errors.push(error);
        object()
    });
    propagate_usage(&schemas, &mut usage);
    tag_features(&mut schemas, &usage);
    mark_speech_branch(&mut schemas);
    let mut document = json!({
        "openapi": "3.1.0",
        "info": match describe::info() {
            Ok(info) => info,
            Err(error) => {
                errors.push(error);
                Value::Null
            }
        },
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": match describe::security_schemes() {
                Ok(security) => security,
                Err(error) => {
                    errors.push(error);
                    Value::Object(object())
                }
            }
        },
        "tags": match describe::tags(&operations) {
            Ok(tags) => Value::Array(tags),
            Err(error) => {
                errors.push(error);
                Value::Array(Vec::new())
            }
        }
    });
    if let Err(found) = describe::check_loose(&document) {
        errors.extend(found);
    }
    describe::mark_pass_through(&mut document);
    if let Err(found) = describe::check_refs(&document) {
        errors.extend(found);
    }
    if errors.is_empty() {
        Ok(document)
    } else {
        Err(errors)
    }
}

fn generator(contract: Contract) -> SchemaGenerator {
    let mut settings = SchemaSettings::draft2020_12();
    settings.definitions_path = "/components/schemas".into();
    settings.meta_schema = None;
    settings.contract = match contract {
        Contract::Request => schemars::generate::Contract::Deserialize,
        Contract::Response => schemars::generate::Contract::Serialize,
    };
    settings.into_generator()
}

fn build_operation(
    operation: &Operation,
    request: &mut SchemaGenerator,
    response: &mut SchemaGenerator,
    usage: &mut Usage,
) -> Result<(String, String, Value), String> {
    let description = fields::handler_doc(operation)?;
    let mut value = json!({
        "description": description,
        "operationId": operation.id,
        "responses": success_and_errors(operation, request, response, usage)?,
        "tags": [operation.tag],
        "x-runvil-source": format!("{}#{}", operation.file, operation.handler)
    });
    let object = value.as_object_mut().expect("an operation");
    if let Some(standard) = operation.standard {
        object.insert("x-runvil-standard".to_owned(), json!(standard));
    } else {
        object.insert("x-runvil-extension".to_owned(), json!(true));
        object.insert("x-runvil-opt-in".to_owned(), json!(false));
    }
    if let Some(feature) = operation.feature {
        object.insert("x-runvil-feature".to_owned(), json!(feature));
    }
    if operation.authed {
        object.insert("x-runvil-loopback-trust".to_owned(), json!(true));
        object.insert(
            "security".to_owned(),
            json!([{ "bearer": [] }, { "sessionCookie": [] }]),
        );
    } else {
        object.insert("security".to_owned(), json!([]));
    }
    if !operation.extension_fields.is_empty() {
        object.insert(
            "x-runvil-extension-fields".to_owned(),
            Value::Array(fields::extension_fields(operation.extension_fields)?),
        );
    }
    if operation.websocket {
        object.insert("x-runvil-websocket".to_owned(), json!(true));
    }
    let mut parameters = Vec::new();
    if operation.client_header {
        parameters.push(client_header()?);
    }
    if let Some(name) = operation.path_param {
        parameters.push(json!({
            "description": fields::labeled_paragraph(operation.file, operation.handler, name)?,
            "in": "path",
            "name": name,
            "required": true,
            "schema": { "type": "string" }
        }));
    }
    if operation.websocket {
        parameters.push(intent_parameter()?);
    }
    if !parameters.is_empty() {
        object.insert("parameters".to_owned(), Value::Array(parameters));
    }
    if let Some(body) = &operation.body {
        let media = media_entry(body, request, response, usage, operation.feature)?;
        object.insert(
            "requestBody".to_owned(),
            json!({
                "content": { body.media: media },
                "required": true
            }),
        );
    }
    Ok((
        operation.path.to_owned(),
        operation.method.as_str().to_ascii_lowercase(),
        value,
    ))
}

fn success_and_errors(
    operation: &Operation,
    request: &mut SchemaGenerator,
    response: &mut SchemaGenerator,
    usage: &mut Usage,
) -> Result<Map<String, Value>, String> {
    let mut responses = if operation.websocket {
        errors::realtime_responses()?
    } else if operation.feature == Some("stt") && operation.method == Method::POST {
        errors::batch_responses()?
    } else {
        object()
    };
    let mut variants = operation.errors.to_vec();
    if operation.authed {
        variants.push(errors::Variant::Unauthorized);
    }
    for (key, value) in errors::responses(&variants)? {
        merge_response(&mut responses, key, value)?;
    }
    merge_response(&mut responses, "403".to_owned(), errors::host_wall()?)?;
    let success = if operation.websocket {
        merge_response(&mut responses, "403".to_owned(), errors::realtime_origin()?)?;
        success::upgrade()?
    } else {
        let mut content = object();
        for entry in &operation.success {
            let media = media_entry(entry, request, response, usage, operation.feature)?;
            content.insert(entry.media.to_owned(), media);
        }
        json!({
            "content": content,
            "description": fields::handler_doc(operation)?
        })
    };
    merge_response(
        &mut responses,
        operation.success_status.to_string(),
        success,
    )?;
    Ok(responses)
}

fn media_entry(
    content: &Content,
    request: &mut SchemaGenerator,
    response: &mut SchemaGenerator,
    usage: &mut Usage,
    feature: Option<&'static str>,
) -> Result<Value, String> {
    if content.multipart {
        return multipart_body();
    }
    let generator = match content.contract {
        Contract::Request => request,
        Contract::Response => response,
    };
    let schema = (content.schema)(generator);
    record_refs(&schema, feature, usage);
    for event in content.events {
        usage
            .entry((*event).to_owned())
            .or_default()
            .insert(feature);
    }
    let mut entry = json!({ "schema": schema });
    if let Some((file, item)) = content.doc {
        let doc = source::doc(file, item).map_err(|error| error.message(file, item))?;
        entry
            .as_object_mut()
            .expect("a media entry")
            .insert("x-runvil-events".to_owned(), json!(doc));
    }
    if !content.events.is_empty() {
        entry
            .as_object_mut()
            .expect("a media entry")
            .insert("x-runvil-event-schemas".to_owned(), json!(content.events));
    }
    Ok(entry)
}

fn multipart_body() -> Result<Value, String> {
    let file = "crates/gateway/stt/api/src/batch.rs";
    let fields = source::multipart_fields(file)?;
    let text = std::fs::read_to_string(source::root().join(file))
        .map_err(|error| format!("{file}: {error}"))?;
    let mut required = Vec::new();
    for field in &fields {
        if text.contains(&format!("MissingField(\"{field}\")")) {
            required.push(field.clone());
        }
    }
    let mut properties = object();
    for field in &fields {
        properties.insert(
            field.clone(),
            json!({
                "description": fields::labeled_paragraph(file, "parse_form", field)?,
                "type": "string"
            }),
        );
    }
    Ok(json!({
        "schema": {
            "properties": properties,
            "required": required,
            "type": "object"
        }
    }))
}

fn client_header() -> Result<Value, String> {
    let file = "crates/gateway/routing/src/queue-client.rs";
    let doc = source::doc(file, "ClientId").map_err(|error| error.message(file, "ClientId"))?;
    Ok(json!({
        "description": doc,
        "in": "header",
        "name": "X-Runvil-Client",
        "required": false,
        "schema": { "type": "string" },
        "x-runvil-extension": true,
        "x-runvil-opt-in": fields::client_opt_in(&doc)?
    }))
}

fn intent_parameter() -> Result<Value, String> {
    let file = "crates/gateway/stt/api/src/realtime/query.rs";
    let doc = source::module_doc(file).map_err(|error| error.message(file, "module"))?;
    let text = std::fs::read_to_string(source::root().join(file))
        .map_err(|error| format!("{file}: {error}"))?;
    let Some(accepted) = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Some(\"")
            .and_then(|rest| rest.strip_suffix("\") => Ok(()),"))
    }) else {
        return Err(format!("{file}: no accepted intent"));
    };
    Ok(json!({
        "description": doc,
        "in": "query",
        "name": "intent",
        "required": true,
        "schema": { "enum": [accepted], "type": "string" }
    }))
}

fn merge_definitions(
    request: &mut SchemaGenerator,
    response: &mut SchemaGenerator,
) -> Result<Map<String, Value>, String> {
    let mut schemas = request.take_definitions(true);
    for (name, schema) in response.take_definitions(true) {
        if let Some(existing) = schemas.get(&name)
            && existing != &schema
        {
            return Err(format!(
                "ContractConflict: `{name}` has different request and response schemas"
            ));
        }
        schemas.entry(name).or_insert(schema);
    }
    schemas.insert("ErrorEnvelope".to_owned(), errors::envelope_schema()?);
    let names: Vec<String> = schemas.keys().cloned().collect();
    for name in &names {
        let stem = name.trim_end_matches(|char: char| char.is_ascii_digit());
        if stem != name && schemas.contains_key(stem) {
            return Err(format!(
                "SchemaNameCollision: `{name}` was renamed beside `{stem}`"
            ));
        }
    }
    Ok(schemas)
}

pub(super) fn record_refs(schema: &Value, feature: Option<&'static str>, usage: &mut Usage) {
    for name in refs_in(schema) {
        usage.entry(name).or_default().insert(feature);
    }
}

pub(super) fn refs_in(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    walk_refs(value, &mut names);
    names
}

fn walk_refs(value: &Value, names: &mut Vec<String>) {
    if let Some(reference) = value.get("$ref").and_then(Value::as_str)
        && let Some(name) = reference.strip_prefix("#/components/schemas/")
    {
        names.push(name.to_owned());
    }
    match value {
        Value::Object(object) => {
            for child in object.values() {
                walk_refs(child, names);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk_refs(child, names);
            }
        }
        _ => {}
    }
}

fn propagate_usage(schemas: &Map<String, Value>, usage: &mut Usage) {
    loop {
        let mut changed = false;
        let snapshot = usage.clone();
        for (name, schema) in schemas {
            let Some(features) = snapshot.get(name) else {
                continue;
            };
            for child in refs_in(schema) {
                let entry = usage.entry(child).or_default();
                for feature in features {
                    changed |= entry.insert(*feature);
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn tag_features(schemas: &mut Map<String, Value>, usage: &Usage) {
    for (name, features) in usage {
        let mut present = Vec::new();
        for feature in features {
            let Some(feature) = feature else {
                present.clear();
                break;
            };
            present.push(*feature);
        }
        if present.len() == 1
            && features.len() == 1
            && let Some(schema) = schemas.get_mut(name)
            && let Some(object) = schema.as_object_mut()
        {
            object.insert("x-runvil-feature".to_owned(), json!(present[0]));
        }
    }
}

fn mark_speech_branch(schemas: &mut Map<String, Value>) {
    let Some(schema) = schemas.get_mut("CatalogModelInfo") else {
        return;
    };
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = schema.get_mut(key) else {
            continue;
        };
        for branch in branches {
            let marks = branch
                .get("$ref")
                .and_then(Value::as_str)
                .is_some_and(|reference| reference.ends_with("/SpeechCatalogModelInfo"));
            if marks {
                branch
                    .as_object_mut()
                    .map(|object| object.insert("x-runvil-feature".to_owned(), json!("stt")));
            }
        }
    }
}
