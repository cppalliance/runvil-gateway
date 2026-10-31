//! The sheet schema check: the committed `sheet.schema.json` must equal the
//! schema derived from the serde types, and the difference between them is
//! classified before the artifact is accepted.
//!
//! `BLESS_SCHEMA=1 cargo test -p gateway-api-types --test sheet_schema`
//! rewrites the artifact. A breaking difference is refused until
//! `ACCEPTED_SHEET_SCHEMA_VERSION` has been raised, and raising that
//! constant for an additive difference is refused too.

#[path = "sheet_schema/classify.rs"]
mod classify;

use std::env;
use std::fs;
use std::path::PathBuf;

use gateway_api_types::{ACCEPTED_SHEET_SCHEMA_VERSION, Sheet};
use serde_json::Value;

use classify::Comparison;

/// The one command that rewrites the artifact.
const REGENERATE: &str = "BLESS_SCHEMA=1 cargo test -p gateway-api-types --test sheet_schema";

/// The name the root schema's own properties are reported under.
const ROOT: &str = "Sheet";

/// The committed artifact, resolved from the manifest so the check does not
/// depend on the working directory.
fn artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sheet.schema.json")
}

/// The schema derived from the types.
fn derived_schema() -> Value {
    schemars::schema_for!(Sheet).to_value()
}

/// `value` with every object's keys in sorted order, so the artifact renders
/// the same whatever order the generator and the JSON map give.
fn canonical(value: &Value) -> Value {
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

/// The artifact text: two-space indent, sorted keys, a trailing newline, and
/// no `$schema` URL, so a schemars bump does not churn the file.
fn render(schema: &Value) -> String {
    let mut value = canonical(schema);
    if let Value::Object(object) = &mut value {
        object.remove("$schema");
    }
    let mut text = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|error| panic!("the derived schema must serialize: {error}"));
    text.push('\n');
    text
}

/// The version a schema stamps as the `const` under `schema_version`.
fn stamped_version(schema: &Value) -> Option<u64> {
    schema
        .get("properties")?
        .get("schema_version")?
        .get("const")?
        .as_u64()
}

/// Fails unless the derivation's version and the constant this reader accepts
/// are the same number.
fn check_stamped_version(derived: u64, accepted: u64) {
    assert_eq!(
        derived, accepted,
        "the `const` on `schema_version` and `ACCEPTED_SHEET_SCHEMA_VERSION` must be the same \
         number: the derivation stamps {derived} and the constant is {accepted}. Change the \
         literal in the `extend` attribute beside the constant, then rewrite the artifact \
         with: {REGENERATE}"
    );
}

/// Fails when the change and the version constant disagree about what the
/// change costs.
fn check_classification(comparison: &Comparison, committed: u64, accepted: u64) {
    let bumped = accepted > committed;
    let breaking = comparison.is_breaking();
    assert!(
        !breaking || bumped,
        "the change is breaking, so `ACCEPTED_SHEET_SCHEMA_VERSION` must be bumped from \
         {accepted} and the artifact regenerated. {}",
        comparison.report()
    );
    assert!(
        breaking || !bumped,
        "the change is additive: it does not need a version bump, and a gratuitous bump \
         strands every shipped Gateway, because they gate on equality. \
         `ACCEPTED_SHEET_SCHEMA_VERSION` is {accepted} and the artifact stamps {committed}; \
         revert the constant to {committed}. {}",
        comparison.report()
    );
}

#[test]
fn the_committed_schema_matches_the_derived_one() {
    let derived = derived_schema();
    let derived_text = render(&derived);
    let path = artifact_path();
    let accepted = u64::from(ACCEPTED_SHEET_SCHEMA_VERSION);
    let derived_version = stamped_version(&derived)
        .unwrap_or_else(|| panic!("the derivation must stamp a `const` on `schema_version`"));
    let bless = env::var_os("BLESS_SCHEMA").is_some();

    let committed_text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                bless,
                "{} is missing; write it with: {REGENERATE}",
                path.display()
            );
            fs::write(&path, &derived_text)
                .unwrap_or_else(|error| panic!("{}: unwritable: {error}", path.display()));
            return;
        }
        Err(error) => panic!("{}: unreadable: {error}", path.display()),
    };

    let committed: Value = serde_json::from_str(&committed_text)
        .unwrap_or_else(|error| panic!("{}: unparseable: {error}", path.display()));
    let committed_version = stamped_version(&committed).unwrap_or_else(|| {
        panic!(
            "{}: `properties.schema_version` has no `const`, so the artifact stamps no \
             version. Rewrite it with: {REGENERATE}",
            path.display()
        )
    });

    check_stamped_version(derived_version, accepted);
    let comparison = Comparison::between(&committed, &derived);
    check_classification(&comparison, committed_version, accepted);

    if committed_text == derived_text {
        return;
    }
    if bless {
        fs::write(&path, &derived_text)
            .unwrap_or_else(|error| panic!("{}: unwritable: {error}", path.display()));
        return;
    }
    panic!(
        "{} is stale: it does not match the schema derived from the types. {}\n\
         Rewrite it with: {REGENERATE}",
        path.display(),
        comparison.report()
    );
}
