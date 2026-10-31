//! The classifier's verdicts on the differences a sheet change can make, and
//! the report each one prints.

use serde_json::json;

use super::*;

/// The definition shape the tests edit: one required string property and one
/// enum property, both required.
fn baseline() -> Value {
    json!({
        "$defs": {
            "Entry": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "kind": { "type": "string", "enum": ["chat", "embedding"] }
                },
                "required": ["id", "kind"]
            }
        },
        "properties": { "schema_version": { "type": "integer", "const": 1 } },
        "required": ["schema_version"]
    })
}

#[test]
fn an_unchanged_artifact_has_no_differences() {
    let comparison = Comparison::between(&baseline(), &baseline());
    assert!(!comparison.is_breaking());
    assert!(
        comparison
            .report()
            .contains("no property, `required` entry"),
        "{}",
        comparison.report()
    );
}

#[test]
fn a_rename_classifies_as_breaking() {
    let mut derived = baseline();
    let entry = derived["$defs"]["Entry"]
        .as_object_mut()
        .expect("an object");
    let properties = entry["properties"].as_object_mut().expect("an object");
    properties.remove("id");
    properties.insert("identifier".to_owned(), json!({ "type": "string" }));
    let renamed = json!("identifier");
    for name in entry["required"].as_array_mut().expect("an array") {
        if name.as_str() == Some("id") {
            *name = renamed.clone();
        }
    }

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(
        comparison.is_breaking(),
        "a rename must be breaking: {}",
        comparison.report()
    );
    let report = comparison.report();
    assert!(report.contains("`id` is removed or renamed"), "{report}");
    assert!(report.contains("`identifier` is added"), "{report}");
    assert!(
        report.contains("the change classifies as breaking"),
        "{report}"
    );
}

#[test]
fn a_property_outside_required_classifies_as_additive() {
    let mut derived = baseline();
    derived["$defs"]["Entry"]["properties"]["variant"] = json!({ "type": "string" });

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(
        !comparison.is_breaking(),
        "a property outside `required` is additive: {}",
        comparison.report()
    );
    let report = comparison.report();
    assert!(
        report.contains("the change classifies as additive"),
        "{report}"
    );
    assert!(
        report.contains("`variant` is added, not required"),
        "{report}"
    );
}

#[test]
fn a_property_added_to_required_classifies_as_breaking() {
    let mut derived = baseline();
    derived["$defs"]["Entry"]["properties"]["family"] = json!({ "type": "string" });
    derived["$defs"]["Entry"]["required"]
        .as_array_mut()
        .expect("an array")
        .push(json!("family"));

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(comparison.is_breaking(), "{}", comparison.report());
    assert!(
        comparison
            .report()
            .contains("`family` is added to `required`"),
        "{}",
        comparison.report()
    );
}

#[test]
fn a_removed_enum_variant_classifies_as_breaking() {
    let mut derived = baseline();
    derived["$defs"]["Entry"]["properties"]["kind"]["enum"] = json!(["chat", "video"]);

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(comparison.is_breaking(), "{}", comparison.report());
    let report = comparison.report();
    assert!(report.contains("`embedding` is removed"), "{report}");
    assert!(report.contains("`video` is added"), "{report}");
}

#[test]
fn a_changed_format_classifies_as_breaking() {
    let mut derived = baseline();
    derived["$defs"]["Entry"]["properties"]["id"]["format"] = json!("date-time");

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(comparison.is_breaking(), "{}", comparison.report());
    assert!(
        comparison.report().contains("changed `format`"),
        "{}",
        comparison.report()
    );
}

#[test]
fn a_changed_type_classifies_as_breaking() {
    let mut derived = baseline();
    derived["$defs"]["Entry"]["properties"]["id"]["type"] = json!("integer");

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(comparison.is_breaking(), "{}", comparison.report());
    assert!(
        comparison.report().contains("changed `type` from"),
        "{}",
        comparison.report()
    );
}

#[test]
fn a_one_of_arm_constant_is_read_as_a_variant() {
    let mut committed = baseline();
    committed["$defs"]["Entry"]["properties"]["kind"] = json!({
        "oneOf": [
            { "type": "string", "const": "chat" },
            { "type": "string", "const": "embedding" }
        ]
    });
    let mut derived = committed.clone();
    derived["$defs"]["Entry"]["properties"]["kind"] = json!({
        "oneOf": [
            { "type": "string", "const": "chat" },
            { "type": "string", "const": "embedding" },
            { "type": "string", "const": "video" }
        ]
    });

    let comparison = Comparison::between(&committed, &derived);
    assert!(
        !comparison.is_breaking(),
        "a new `oneOf` arm is an added variant: {}",
        comparison.report()
    );
    assert!(
        comparison.report().contains("`video` is added"),
        "{}",
        comparison.report()
    );
}

#[test]
fn a_removed_definition_classifies_as_breaking() {
    let mut derived = baseline();
    derived["$defs"]
        .as_object_mut()
        .expect("an object")
        .remove("Entry");

    let comparison = Comparison::between(&baseline(), &derived);
    assert!(comparison.is_breaking(), "{}", comparison.report());
    assert!(
        comparison
            .report()
            .contains("the definition `Entry` is removed"),
        "{}",
        comparison.report()
    );
}
