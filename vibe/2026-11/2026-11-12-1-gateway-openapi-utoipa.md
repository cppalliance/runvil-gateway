---
name: Gateway OpenAPI description with utoipa
overview: "Describe the Gateway's public HTTP surface as an OpenAPI 3.1 document produced by utoipa, from #[utoipa::path] attributes on the handlers and ToSchema derives on the wire types. Pin it as crates/gateway/app/openapi.json with drift, coverage, feature-shape, and validity tests, so the result can be compared with the hand-built generator that reads the same surface from the source."
todos:
  - id: utoipa-description
    content: Annotate the 13 operations and their wire types with utoipa, assemble and pin the document, and add the drift, coverage, feature, and validity tests
    status: completed
isProject: false
---

# Describe the Gateway API with utoipa

<product-contract>

## Product Requirements

The Gateway has no machine-readable description of its API, and `crates/gateway/README.md` asks for one generated from the source and checked by CI. A reviewer asked on the hand-built description's pull request whether a third-party crate could provide it. This plan builds the same description on utoipa, for comparison, and records what utoipa can and cannot express for this server.

- Problem and users:
  - Users are the reviewer comparing the two approaches, the maintainers who add routes and wire fields, and every client written against the Gateway.
  - The same public surface as the hand-built description: 13 operations on 12 paths, the 11 registry operations plus `POST /v1/audio/transcriptions` and `GET /v1/realtime`.
- Goals:
  - An OpenAPI 3.1 document that utoipa produces from attributes on the handlers and derives on the wire types.
  - The document committed as `crates/gateway/app/openapi.json`, the same path as the hand-built one, so the two files diff directly.
  - The project's conventions kept where utoipa can carry them, and every gap recorded.
- Non-goals:
  - Serving the document from the Gateway.
  - Documenting the admin tiers or the config UI assets.
  - Describing WebSocket or server-sent event messages beyond prose.
  - Changing any status code, wire format, or handler logic.
- Success criteria:
  - All 13 operations are present, and every mounted route is documented or excluded with a reason.
  - The committed file equals a fresh generation, and `BLESS_OPENAPI=1` rewrites it.
  - The document validates offline against the official OAS 3.1 schema.
  - `--no-default-features` builds, and its OpenAPI tests pass.
- Constraints:
  - Workspace lints deny clippy `all` and `pedantic`, `unwrap_used`, and `expect_used`; `CARGO_BUILD_WARNINGS=deny` promotes warnings.
  - Every `.rs` file stays at or under 500 lines (`crates/build-ceiling/src/lib.rs`).
  - `cargo deny check` must pass with the new dependencies, without editing `deny.toml`.
- Open questions:
  - Whether the reviewer prefers the utoipa annotations on the runtime items, or the test-only generator with its source-derived provenance.

## Functional Specification

The document is generated in a test build and compared with the committed file. Each route's description lives on its handler; a test-only module lists the handlers and adds what the attributes cannot carry.

- Actors and workflows:
  - A maintainer who adds or changes a route edits the `#[utoipa::path]` attribute beside the handler, adds a new handler to the list in `crates/gateway/app/src/openapi.rs`, and runs `BLESS_OPENAPI=1 cargo test -p gateway --lib openapi`.
  - A maintainer who adds a wire field adds a doc comment, and a `#[schema(...)]` attribute where the field is an extension.
- Inputs and outputs:
  - Input: the 13 handler attributes, `ToSchema` derives on 40 types, the `OpenApi` derives, and the marker tables in `crates/gateway/app/src/openapi/markers.rs`.
  - Output: `crates/gateway/app/openapi.json`, sorted keys, two-space indent, trailing newline.
- States and validation:
  - Operations carry `x-runvil-standard` or `x-runvil-extension` with `x-runvil-opt-in`, `x-runvil-feature`, `x-runvil-loopback-trust`, and on `/v1/realtime` `x-runvil-websocket`.
  - Error responses carry `x-runvil-error-codes`; loose schemas carry `x-runvil-pass-through`.
- Errors and recovery:
  - A marker pointer that no longer resolves fails generation and names the pointer.
  - A stale committed file fails the drift test, lists the changed JSON pointers, and prints the regenerate command.
- Security and privacy behavior:
  - Two security schemes, `bearer` and `sessionCookie`, apply to every operation except `/health`, which declares `security: [{}]`.
  - OpenAPI cannot express loopback trust; the info description states it and the operations that admit it carry `x-runvil-loopback-trust: true`.
- Acceptance criteria:
  - The 8 OpenAPI tests pass in the default build and in a `--no-default-features` build.

</product-contract>
<implementation-contract>

## Technical Design

utoipa's attributes and derives describe the operations and schemas, and its `OpenApi` derive assembles them. The router is untouched; the route registry stays the source of the mounted routes and the coverage test holds the document to it. A JSON post-processing step adds the markers utoipa has no place for.

- Architecture:
  - `#[utoipa::path]` on the 11 app handlers and the 2 speech handlers, each binding its path to the registry constant (`path = CHAT_COMPLETIONS.path`) so the attribute and the router cannot name different paths.
  - `GatewayApi`, `WebSearchApi`, and `CacheApi` (`#[derive(OpenApi)]`, test-only) list the handlers; the feature ones merge in under the same cfg gates that mount them. `gateway_stt::SpeechApi` lists the speech crate's two handlers and merges the same way.
  - A `Modify` adds the two security schemes and the licence identifier.
  - `openapi/markers.rs` adds markers at fixed JSON pointers to the serialized document, widens two `oneOf` unions to `anyOf`, and sets the loading 503's `Retry-After` schema to `const: 5`.
- Modules and interfaces:
  - `crates/gateway/app/src/openapi.rs` and `openapi/markers.rs`, both inside `#[cfg(test)] mod openapi`.
  - `crates/gateway/protocol/src/wire/envelope.rs` adds `ErrorEnvelope` and `ErrorDetail`, schema-only types for the error body three builders write as JSON literals. Both schemas are closed, because the builders write exactly `message`, `type`, and `code`.
- File and public API changes:
  - New dependency `utoipa` 6 (default `macros` feature) in `gateway-api-types`, `gateway-protocol`, `gateway-web-search`, `gateway-local`, `gateway-stt`, and `gateway`; `jsonschema` 0.61 as a dev-dependency of `gateway`.
  - `gateway-stt` gains a direct `gateway-protocol` dependency, already in its graph through `gateway-local`.
  - New public items: `gateway_protocol::wire::{ErrorEnvelope, ErrorDetail}`, `gateway_stt::{SpeechApi, TRANSCRIPTIONS_PATH, REALTIME_PATH}`, `gateway_config_ui::ASSET_PATHS`.
  - The registry gains the two speech routes and the five config UI assets, as in the hand-built description's change.
  - Three `json!` replies become typed structs that serialize byte-identically: `/health`, `/v1/audio/voices`, and the cache delete reply.
  - `crates/gateway/stt/api/src/realtime/route.rs` moves `session_error` into a private child, `route-errors.rs`, because the attribute took the file past 500 lines; `realtime::route` becomes `pub(crate)` so the speech crate's `OpenApi` derive can name the handler.
- Data, persistence, failure, security, and privacy constraints:
  - No route, status code, header, or body changes; the one runtime-observable change is seven more `route mounted` debug lines from the registry addition.

</implementation-contract>
<verification-contract>

## Testing Plan

Eight tests in the `openapi` module hold the document to the code, the registry, and the OAS 3.1 schema. Byte-equality tests pin the typed replies, and a protocol test pins the restated speech bounds.

- Unit:
  - `the_committed_document_matches_this_build`: byte equality in a full build; in a build missing route features, equality with the committed document minus every element marked for a missing feature.
  - `every_mounted_route_is_documented_or_excluded`, `each_operation_carries_the_feature_that_mounts_it`, `every_operation_carries_its_conventions`.
  - `the_document_is_openapi_3_1`, `a_bad_response_code_is_rejected`, `every_open_schema_is_marked_pass_through`, `each_branch_of_an_overlapping_union_validates`.
  - `the_schema_bounds_are_the_bounds_validate_enforces` in `gateway-protocol`; three byte-equality tests for the typed replies.
- Integration and end-to-end:
  - None beyond the existing suite, which runs unchanged.
- Regression, security, and performance:
  - The existing workspace suite passes, and `registry-tests.rs` sweeps the seven added routes.
- Exit criteria:
  - `cargo fmt --all --check`
  - `CARGO_BUILD_WARNINGS=deny cargo clippy --locked --workspace --all-targets --all-features`
  - `cargo nextest run --locked --workspace --all-features`
  - `cargo check --locked -p gateway --no-default-features`
  - `cargo test --locked -p gateway --lib --no-default-features openapi`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --all-features`
  - `cargo deny check`, `cargo xtask tidy`, `cargo test -p build-xtask`
  - `npm run typecheck` and `npm test` in `crates/gateway/config-ui/ui`

</verification-contract>
<decision-record>

## Decision Record

- Decisions:
  - Annotations with an `OpenApi` derive list, not utoipa-axum's `OpenApiRouter`. utoipa documents both; the derive list is its core example for every framework, and `OpenApiRouter` would replace `Router` in every area module, move path ownership from the registry into the attributes, and could not reach the speech crate's router or the nested config UI service. This choice changes the least runtime code.
  - Unconditional derives and attributes, with `utoipa` a normal dependency. The alternative, `cfg_attr(feature = "openapi", ...)` on every derive and field attribute with the feature enabled by the test build, keeps utoipa out of the release binary at the cost of an `openapi` feature in six crates and gates on the schema-only types.
  - The document assembly is test-only, like the hand-built generator, so the release binary carries the derives but never builds the document.
  - Error responses are declared per handler with `body = ErrorEnvelope`; the codes are machine-readable through `x-runvil-error-codes`, added by the post-processing step, because utoipa's response attribute has no extensions.
  - The registry addition reuses the hand-built branch's change: the two speech path constants, the config UI asset paths, and the registry test.
  - The vendored OAS 3.1 schemas and the validity test approach are reused from the hand-built branch.
- Rejected alternatives:
  - utoipa-axum's `OpenApiRouter`, for the reasons above. Revisit if the Gateway adopts utoipa for serving and wants one list of routes.
  - Schema-only types behind `#[cfg(test)]`, which cannot work, because the unconditional attributes name them in every build.
  - OpenAPI 3.2, whose `itemSchema` would describe the server-sent event lines and which utoipa 6 can emit. Revisit when the hand-built description moves to 3.2 too, so the comparison stays like for like.
- Assumptions, risks, and notes:
  - utoipa cannot express, and the document handles each as stated: `const` (the post-processing step sets the `Retry-After` header to `const: 5`; enums are `enum` lists without per-variant descriptions); extensions on a response, a media type, a whole component, a `$ref` field, or an any-value schema (added by the post-processing step); extensions on a list field, which it moves onto the items (added at the property by the post-processing step); a constant bound in `max_length` or `minimum` (restated as literals and pinned by a protocol test); separate request and response contracts (always-written `#[serde(default)]` response fields carry `required = true` by hand).
  - utoipa writes every untagged enum as `oneOf`. For `CatalogModelInfo` and `TranscriptionResponse` a valid body matches two branches, so `oneOf` rejects every inference model and every `verbose_json` reply; the post-processing step widens both to `anyOf`, and a test validates a body of each branch.
  - Five schema-only types are never constructed and carry `#[expect(dead_code)]` or are public: `ErrorEnvelope`, `ErrorDetail`, `TranscriptionUpload`, `RealtimeIntent`, and `CacheReadyReply`. Nothing but review ties them to the JSON the server writes.
  - No test checks that a declared status or error code is one the route can return, or that a real response matches its schema; the hand-built branch reads statuses from `GatewayError::into_response` and has a conformance test.
  - Descriptions are the handlers' doc comments as written, so maintainer notes and rustdoc links reach the document; utoipa puts the first paragraph in `summary`.

### Deferred and Out of Scope

- Deferred: not done here, a conformance test. No test sends a request or builds a `GatewayError` envelope and checks the real response against the document. Revisit before this is proposed for merge.
- Deferred: not done here, a check on the error codes. The `ERROR_CODES` table in `openapi/markers.rs` is written by hand and was copied from the hand-built branch, and tests check only that each JSON error response carries codes. Revisit with the conformance test.

</decision-record>
<project-survey>

## Project Survey

- Status: complete.
- Base: `origin/master` at `b413dbb`. Toolchain 1.99 from `rust-toolchain.toml`.
- Versions: utoipa 6.0.0 and utoipa-gen 6.0.1 (the newest on crates.io); jsonschema 0.61.2. utoipa-axum 0.3.0 was read, not used.
- Conventions: kebab `-tests.rs` siblings wired with `#[path]`; area modules bind their registry constant's path; every crate's build script runs the 500-line ceiling.

</project-survey>
<execution-plan>

## Execution Instructions

- Work items: Step 1.
- Dependencies and verification: the exit criteria in the Testing Plan.

<step-1>

### Step 1: Describe the Gateway API with utoipa [completed]

- Artifacts: the utoipa annotations on 13 handlers and 40 types, `crates/gateway/app/src/openapi.rs`, `openapi/markers.rs`, the two test files, `crates/gateway/app/openapi.json`, the vendored OAS schemas, the registry addition, the README notes, and one CI step that runs the OpenAPI tests without default features.
- Behavior: none changes at run time apart from seven more `route mounted` debug lines.
- Tests: the Testing Plan.
- Commit: `Describe the Gateway API with utoipa`

</step-1>

</execution-plan>
