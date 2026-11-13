//! Extension fields that live inside a pass-through value, and the doc
//! comments and labeled paragraphs that describe an operation's parts.

use serde_json::{Value, json};

use super::operations::Operation;
use super::source;

/// An extension that has no typed property, so the operation names it.
pub(super) struct ExtensionField {
    /// The path the field has on the wire.
    pub name: &'static str,
    /// The file the description is read from.
    pub file: &'static str,
    /// An item doc, or the module paragraph containing this text.
    pub doc: FieldDoc,
    /// The item whose doc names the opt-in. Absent means the field is not opt-in.
    pub opt_in: Option<&'static str>,
}

/// Where an extension field's description is written.
pub(super) enum FieldDoc {
    /// The doc comment of this item.
    #[cfg(feature = "stt")]
    Item(&'static str),
    /// The module-doc paragraph that contains this text.
    ModuleParagraph(&'static str),
}

/// The item's doc comment. A missing doc fails generation.
pub(super) fn documented(file: &str, item: &str) -> Result<String, String> {
    source::doc(file, item).map_err(|error| error.message(file, item))
}

/// The paragraph of `item`'s doc comment that opens with `` `label`: ``,
/// without the label. It describes one part of the item that has no doc
/// comment of its own: a multipart field, a path capture, an error code, or
/// a status. A missing paragraph fails generation.
pub(super) fn labeled_paragraph(file: &str, item: &str, label: &str) -> Result<String, String> {
    let doc = source::doc(file, item).map_err(|error| error.message(file, item))?;
    labeled(&doc, label)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{file}: no paragraph of `{item}`'s doc opens with `{label}`"))
}

/// The paragraph of `doc` that opens with `` `label`: ``, without the label.
pub(super) fn labeled<'doc>(doc: &'doc str, label: &str) -> Option<&'doc str> {
    let prefix = format!("`{label}`: ");
    doc.split("\n\n")
        .find_map(|paragraph| paragraph.strip_prefix(prefix.as_str()))
}

/// The handler's doc comment without the labeled paragraphs that its path
/// capture and its realtime refusals publish on their own.
pub(super) fn handler_doc(operation: &Operation) -> Result<String, String> {
    let doc = documented(operation.file, operation.handler)?;
    let mut labels: Vec<String> = operation
        .path_param
        .map(ToOwned::to_owned)
        .into_iter()
        .collect();
    if operation.websocket {
        labels.extend(super::errors::realtime_codes()?);
    }
    Ok(doc
        .split("\n\n")
        .filter(|paragraph| {
            labels
                .iter()
                .all(|label| labeled(paragraph, label).is_none())
        })
        .collect::<Vec<_>>()
        .join("\n\n"))
}

pub(super) fn extension_fields(fields: &[ExtensionField]) -> Result<Vec<Value>, String> {
    fields.iter().map(extension_field).collect()
}

fn extension_field(field: &ExtensionField) -> Result<Value, String> {
    let description = match field.doc {
        #[cfg(feature = "stt")]
        FieldDoc::Item(item) => documented(field.file, item)?,
        FieldDoc::ModuleParagraph(needle) => module_paragraph(field.file, needle)?,
    };
    let opt_in = match field.opt_in {
        Some(item) => Value::String(documented(field.file, item)?),
        None => json!(false),
    };
    Ok(json!({
        "description": description,
        "name": field.name,
        "x-runvil-extension": true,
        "x-runvil-opt-in": opt_in
    }))
}

fn module_paragraph(file: &str, needle: &str) -> Result<String, String> {
    let doc = source::module_doc(file).map_err(|error| error.message(file, "module"))?;
    doc.split("\n\n")
        .find(|paragraph| paragraph.contains(needle))
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{file}: no module paragraph contains `{needle}`"))
}

/// The sentence in `ClientId`'s doc that names the header the client chooses to send.
pub(super) fn client_opt_in(doc: &str) -> Result<String, String> {
    let flat = doc.replace('\n', " ");
    flat.split(". ")
        .find(|sentence| sentence.contains("X-Runvil-Client"))
        .map(|sentence| format!("{sentence}."))
        .ok_or_else(|| "ClientId's doc does not name X-Runvil-Client".to_owned())
}
