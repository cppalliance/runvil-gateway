//! Success responses a handler sends without a typed JSON body: the speech
//! route's media types and the realtime route's WebSocket upgrade.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::operations::{Content, Contract};
use super::{schemas, source};
use crate::speech::speech_fallback_mime;
use crate::wire::{SpeechResponseFormat, SpeechStreamFormat};

const SPEECH: &str = "crates/gateway/app/src/speech.rs";
const REALTIME: &str = "crates/gateway/stt/api/src/realtime/route.rs";
const CLIENT_EVENTS: &str = "crates/gateway/stt/api/src/realtime/wire/client.rs";
const SERVER_EVENTS: &str = "crates/gateway/stt/api/src/realtime/wire/server.rs";

/// One media entry per `Content-Type` [`speech_fallback_mime`] labels a
/// speech response with, over every format and framing a request can name.
/// A valid upstream `Content-Type` is forwarded instead, as the `relay_audio`
/// doc on each entry says.
pub(super) fn speech_audio() -> Result<Vec<Content>, String> {
    let mut framings = vec![None];
    framings.extend(variants::<SpeechStreamFormat>()?.into_iter().map(Some));
    let mut media = BTreeSet::new();
    for format in variants::<SpeechResponseFormat>()? {
        for framing in &framings {
            media.insert(speech_fallback_mime(format, *framing));
        }
    }
    Ok(media
        .into_iter()
        .map(|media| Content {
            media,
            schema: schemas::audio_schema,
            contract: Contract::Response,
            doc: Some((SPEECH, "relay_audio")),
            events: &[],
            multipart: false,
        })
        .collect())
}

/// The realtime route's `101 Switching Protocols`. It has no HTTP body, and
/// OpenAPI cannot describe the frames that follow, so the description names
/// the modules that define the client and server events.
pub(super) fn upgrade() -> Result<Value, String> {
    let client = source::module_doc(CLIENT_EVENTS)
        .map_err(|error| error.message(CLIENT_EVENTS, "module"))?;
    let server = source::module_doc(SERVER_EVENTS)
        .map_err(|error| error.message(SERVER_EVENTS, "module"))?;
    Ok(json!({
        "description": format!(
            "No body: the connection upgrades to a WebSocket of JSON text frames.\n\n\
             Client events, `{CLIENT_EVENTS}`: {client}\n\n\
             Server events, `{SERVER_EVENTS}`: {server}"
        ),
        "x-runvil-source": format!("{REALTIME}#upgrade")
    }))
}

/// Every value `T`'s JSON Schema names, as an `enum` list or as the `const`
/// of each `oneOf` or `anyOf` branch, read back through `T`'s deserializer.
fn variants<T: JsonSchema + DeserializeOwned>() -> Result<Vec<T>, String> {
    let name = T::schema_name();
    let schema = schemars::schema_for!(T).to_value();
    let mut values: Vec<Value> = schema
        .get("enum")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for key in ["oneOf", "anyOf"] {
        let Some(branches) = schema.get(key).and_then(Value::as_array) else {
            continue;
        };
        for branch in branches {
            let Some(value) = branch.get("const") else {
                return Err(format!("{name}: a `{key}` branch names no `const` value"));
            };
            values.push(value.clone());
        }
    }
    if values.is_empty() {
        return Err(format!("{name}: its schema names no values"));
    }
    values
        .into_iter()
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("{name}: {value} does not deserialize: {error}"))
        })
        .collect()
}
