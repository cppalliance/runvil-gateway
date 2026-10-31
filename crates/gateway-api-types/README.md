# Gateway API types

The Gateway's public wire vocabulary. This crate holds the provider model sheet schema, one versioned JSON snapshot of every provider's models that the Gateway downloads at runtime and that a separate repository publishes weekly, plus the model metadata types and the progress snapshot the Gateway streams to its status consumers. It depends on `serde`, `time`, and `schemars`, and on no other workspace crate, so every product crate may depend on it.

## The committed schema

`sheet.schema.json` is generated from the Rust types. It is not written by hand, and a hand edit is overwritten at the next run. Derive the file, do not edit it:

```console
BLESS_SCHEMA=1 cargo test -p gateway-api-types --test sheet_schema
```

`cargo test -p gateway-api-types` compares the committed file with a fresh derivation and fails when they differ. Edit the types, then run the command above to rewrite the artifact in the same change, and commit both.

## The compatibility rule

A second repository defines its own copy of these types, so both copies must stay in step. Two rules keep them there.

**An additive field needs no version bump.** Add the field, mark it `#[serde(default)]`, and regenerate the schema. A reader one release behind ignores a field it does not know. The test classifies the difference as additive and passes.

**A removal or a rename bumps `ACCEPTED_SHEET_SCHEMA_VERSION`.** A removed property, a renamed property, a changed type or format, a property added to `required`, or a removed enum variant is a breaking change. Raise the constant in `src/lib.rs` and the `const` literal in the `extend` attribute beside it, then regenerate the artifact. A breaking change ships to the reader one release before the writer stops emitting the old shape, so no shipped Gateway reads a shape it does not know.

The test enforces both directions. A breaking change with the constant unchanged fails with this message:

```
the change is breaking, so `ACCEPTED_SHEET_SCHEMA_VERSION` must be bumped from 1 and the artifact regenerated. the change classifies as breaking
  breaking: `ProviderSlice`: the property `openai_base_url` is removed or renamed
  additive: `ProviderSlice`: the property `openai_base` is added, not required
regenerate the artifact with: BLESS_SCHEMA=1 cargo test -p gateway-api-types --test sheet_schema
```

A version bump on an additive change fails with this message:

```
the change is additive: it does not need a version bump, and a gratuitous bump strands every shipped Gateway, because they gate on equality. `ACCEPTED_SHEET_SCHEMA_VERSION` is 2 and the artifact stamps 1; revert the constant to 1. the change classifies as additive
  additive: `ProviderSlice`: the property `probe_field` is added, not required
regenerate the artifact with: BLESS_SCHEMA=1 cargo test -p gateway-api-types --test sheet_schema
```

Both failures name the definitions and properties that differ, and print the command that regenerates the artifact.

## Dates

`schemars` has no `time` support, so the four date fields carry an explicit `#[schemars(schema_with = "...")]` beside their serde attributes. Do not remove it: without it the derive reads `#[serde(with = "time::serde::rfc3339")]` as a type path and fails to compile.
