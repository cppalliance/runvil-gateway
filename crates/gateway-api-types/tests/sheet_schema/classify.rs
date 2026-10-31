//! The classifier: how a difference between the committed artifact and the
//! freshly derived schema affects a Gateway one release behind the writer,
//! and the report a failure prints.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::REGENERATE;

/// How a difference affects a Gateway one release behind the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    /// Lawful with no version bump, provided the new field carries
    /// `#[serde(default)]`.
    Additive,
    /// Needs `ACCEPTED_SHEET_SCHEMA_VERSION` raised.
    Breaking,
}

impl Class {
    /// The word the report prints for this class.
    fn label(self) -> &'static str {
        match self {
            Self::Additive => "additive",
            Self::Breaking => "breaking",
        }
    }
}

/// One classified difference.
struct Change {
    /// What the difference means for a lagging reader.
    class: Class,
    /// What changed, naming the definition and the property.
    line: String,
}

/// Every classified difference between the two artifacts.
#[derive(Default)]
pub(super) struct Comparison {
    /// One entry per difference, in the order they were found.
    changes: Vec<Change>,
}

/// A schema keyword as report text, or `absent` when it is not set.
fn keyword(value: Option<&Value>) -> String {
    value.map_or_else(|| "absent".to_owned(), Value::to_string)
}

/// The `$defs` object of a schema, empty when the schema declares none.
fn definitions(schema: &Value) -> BTreeMap<&str, &Value> {
    schema
        .get("$defs")
        .and_then(Value::as_object)
        .map_or_else(BTreeMap::new, |defs| {
            defs.iter()
                .map(|(name, def)| (name.as_str(), def))
                .collect()
        })
}

/// The names a schema lists under `required`.
fn required(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map_or_else(BTreeSet::new, |names| {
            names.iter().filter_map(Value::as_str).collect()
        })
}

/// The variants a schema allows: the `enum` list, or the `const` of each
/// `oneOf` arm, which is how schemars 1.x writes a unit-variant enum.
fn variants(schema: &Value) -> BTreeSet<&str> {
    let listed = schema
        .get("enum")
        .and_then(Value::as_array)
        .map_or_else(BTreeSet::new, |values| {
            values.iter().filter_map(Value::as_str).collect()
        });
    let arms = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .map_or_else(BTreeSet::new, |arms| {
            arms.iter()
                .filter_map(|arm| arm.get("const").and_then(Value::as_str))
                .collect()
        });
    listed.union(&arms).copied().collect()
}

impl Comparison {
    /// Records one difference.
    fn record(&mut self, class: Class, line: String) {
        self.changes.push(Change { class, line });
    }

    /// Whether any recorded difference breaks a lagging reader.
    pub(super) fn is_breaking(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.class == Class::Breaking)
    }

    /// The report a failure prints: the classification, every added and
    /// removed property with the definition it belongs to, and the command
    /// that rewrites the artifact.
    pub(super) fn report(&self) -> String {
        let class = if self.is_breaking() {
            "breaking"
        } else {
            "additive"
        };
        let mut text = format!("the change classifies as {class}");
        if self.changes.is_empty() {
            text.push_str("\n  no property, `required` entry, or enum variant differs");
        }
        for change in &self.changes {
            text.push_str("\n  ");
            text.push_str(change.class.label());
            text.push_str(": ");
            text.push_str(&change.line);
        }
        text.push_str("\nregenerate the artifact with: ");
        text.push_str(REGENERATE);
        text
    }

    /// Classifies every difference between the artifact and the derivation.
    pub(super) fn between(committed: &Value, derived: &Value) -> Self {
        let mut comparison = Self::default();
        comparison.compare_definition(super::ROOT, committed, derived);
        let old_definitions = definitions(committed);
        let new_definitions = definitions(derived);
        let names: BTreeSet<&str> = old_definitions
            .keys()
            .chain(new_definitions.keys())
            .copied()
            .collect();
        for name in names {
            let line = match (old_definitions.get(name), new_definitions.get(name)) {
                (Some(old), Some(new)) => {
                    comparison.compare_definition(name, old, new);
                    continue;
                }
                (Some(_), None) => format!("the definition `{name}` is removed"),
                (None, Some(_)) => format!("the definition `{name}` is added"),
                (None, None) => continue,
            };
            let class = if old_definitions.contains_key(name) {
                Class::Breaking
            } else {
                Class::Additive
            };
            comparison.record(class, line);
        }
        comparison
    }

    /// Classifies the properties, the `required` list, and the `enum` of one
    /// definition.
    fn compare_definition(&mut self, scope: &str, old: &Value, new: &Value) {
        let (old_required, new_required) = (required(old), required(new));
        match (
            old.get("properties").and_then(Value::as_object),
            new.get("properties").and_then(Value::as_object),
        ) {
            (Some(old_properties), Some(new_properties)) => self.compare_properties(
                scope,
                (old_properties, &old_required),
                (new_properties, &new_required),
            ),
            (None, None) => {}
            _ => self.record(
                Class::Breaking,
                format!("`{scope}`: the object shape changed"),
            ),
        }
        self.compare_variants(scope, old, new);
    }

    /// Classifies every property and `required` entry of one definition.
    fn compare_properties(
        &mut self,
        scope: &str,
        (old_properties, old_required): (&Map<String, Value>, &BTreeSet<&str>),
        (new_properties, new_required): (&Map<String, Value>, &BTreeSet<&str>),
    ) {
        for (name, old_property) in old_properties {
            let Some(new_property) = new_properties.get(name) else {
                self.record(
                    Class::Breaking,
                    format!("`{scope}`: the property `{name}` is removed or renamed"),
                );
                continue;
            };
            let (old_type, new_type) = (old_property.get("type"), new_property.get("type"));
            if old_type != new_type {
                self.record(
                    Class::Breaking,
                    format!(
                        "`{scope}`: the property `{name}` changed `type` from {} to {}",
                        keyword(old_type),
                        keyword(new_type)
                    ),
                );
            }
            let (old_format, new_format) = (old_property.get("format"), new_property.get("format"));
            if old_format != new_format {
                self.record(
                    Class::Breaking,
                    format!(
                        "`{scope}`: the property `{name}` changed `format` from {} to {}",
                        keyword(old_format),
                        keyword(new_format)
                    ),
                );
            }
            if !old_required.contains(name.as_str()) && new_required.contains(name.as_str()) {
                self.record(
                    Class::Breaking,
                    format!("`{scope}`: the property `{name}` is added to `required`"),
                );
            }
            self.compare_variants(&format!("{scope}.{name}"), old_property, new_property);
        }
        for name in new_properties.keys() {
            if old_properties.contains_key(name) {
                continue;
            }
            if new_required.contains(name.as_str()) {
                self.record(
                    Class::Breaking,
                    format!("`{scope}`: the property `{name}` is added to `required`"),
                );
            } else {
                self.record(
                    Class::Additive,
                    format!("`{scope}`: the property `{name}` is added, not required"),
                );
            }
        }
    }

    /// Classifies the enum variants of one definition or property.
    fn compare_variants(&mut self, scope: &str, old: &Value, new: &Value) {
        let (old_variants, new_variants) = (variants(old), variants(new));
        if old_variants.is_empty() && new_variants.is_empty() {
            return;
        }
        for variant in new_variants.difference(&old_variants) {
            self.record(
                Class::Additive,
                format!("`{scope}`: the variant `{variant}` is added"),
            );
        }
        for variant in old_variants.difference(&new_variants) {
            self.record(
                Class::Breaking,
                format!("`{scope}`: the variant `{variant}` is removed"),
            );
        }
    }
}

#[cfg(test)]
#[path = "classify-tests.rs"]
mod tests;
