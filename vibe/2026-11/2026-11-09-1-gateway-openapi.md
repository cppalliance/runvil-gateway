---
name: Gateway OpenAPI 3.1 description
overview: "Generate an OpenAPI 3.1 description of every route the Gateway serves from the code that serves it: route paths and methods from the RouteInfo registry, schemas from schemars derives on the wire types, descriptions from doc comments, error statuses from GatewayError's own response path. Commit the document as crates/gateway/app/openapi.json and pin it with coverage, drift, validity, feature-shape, and conformance tests. Phase 1 is this plan; Phase 2 runs only after the decisions below are approved."
todos:
  - id: plan
    content: Survey the code, check the candidate crates, prototype the schema and validation path, and write this plan (Phase 1)
    status: completed
  - id: enumerable-surface
    content: Bring the seven mounts outside the registry (two STT routes, five config UI assets) into registry::all() through path constants their own crates export
    status: completed
  - id: schema-derives
    content: Derive JsonSchema on the in-scope wire types, with the speech length and range bounds read from the constants validate() uses; no serde change
    status: completed
  - id: typed-literals
    content: Replace the in-scope json! reply literals with typed structs that serialize byte-identically, each pinned by a byte-equality test
    status: completed
  - id: generator
    content: Add the test-only generator, the coverage test, the committed openapi.json, and the drift test with BLESS_OPENAPI
    status: completed
  - id: validity
    content: Vendor the OAS 3.1 schemas, add jsonschema as a dev-dependency, and validate the document plus its structural rules
    status: completed
  - id: feature-shapes
    content: Check feature markers in the reduced test build and from registry cfg attributes; add the CI step
    status: completed
  - id: conformance
    content: Validate real route responses against the spec's schemas for every exercised status
    status: completed
  - id: audit-map
    content: Print the audit map, update the READMEs, and run every gate
    status: completed
isProject: false
---

# Generate and pin an OpenAPI 3.1 description of the Gateway

**Status: implemented.** The decisions below are the ones the code follows. Open questions and contradictions are recorded and left unresolved. The audit map at the end names the source line of every operation and component in `crates/gateway/app/openapi.json`.

**How to read the citations.**

Every claim cites `file:line` or `file:start-end`. Paths are relative to the repository root, with these prefixes:

| Prefix | Expands to |
|---|---|
| `app/` | `crates/gateway/app/src/` |
| `protocol/` | `crates/gateway/protocol/src/` |
| `stt/` | `crates/gateway/stt/api/src/` |
| `api-types/` | `crates/gateway-api-types/src/` |
| `web-search/` | `crates/gateway/web-search/src/` |
| `local/` | `crates/gateway/local/src/` |
| `config/` | `crates/gateway/config/src/` |
| `config-ui/` | `crates/gateway/config-ui/src/` |
| `routing/` | `crates/gateway/routing/src/` |
| `loopback/` | `crates/shared-loopback/src/` |
| `axum-0.8.9/`, `axum-core-0.5.6/`, `schemars-1.2.3/`, `schemars_derive-1.2.3/`, `utoipa-6.0.0/`, `utoipa-gen-6.0.1/`, `aide-0.16.0-alpha.4/`, `jsonschema-0.61.0/`, `referencing-0.61.0/`, `serde_json-1.0.151/` | The published source of that crate version: the `.crate` tarball from `static.crates.io`, or, for axum, axum-core, and serde_json (the versions in `Cargo.lock`), the copy `cargo fetch --locked` puts in the cargo registry |

`README.md` alone means the root README; the API principle is always cited by its full path, `crates/gateway/README.md`.

Line numbers are at commit `9369a7d` (HEAD of `master` when this plan was written). The task statement names `b0e9e3f`; that object does not exist in this clone (`git cat-file -t b0e9e3f` fails), so every count here is taken at `9369a7d`.

<product-contract>

## Product Requirements

The Gateway has no machine-readable description of its API. `crates/gateway/README.md:75` asks for one: "a spec generated from the gateway's source rather than written by hand, with CI comparing the two so a route or a field cannot drift from its description." `vibe/2026-09/2026-09-17-3-gateway-app-decomp.md:116` proposes the same thing as a follow-up: "a generated OpenAPI spec pinned by a snapshot test (crates.io-style)."

- Problem and users:
  - Users are Mateo, who audits the document against the source; the agents that change Gateway routes; and every client written against the Gateway, which today learns the wire shape only from code and prose.
  - The route list exists as data only partly. `app/registry.rs:8-11` says "The registry is the crate's one enumerable route list", and `app/lib.rs:32-36` says the same. But seven mounted routes live outside it: the two STT routes (`stt/batch.rs:98`, `stt/realtime/route.rs:122`, merged at `app/lib.rs:460-469`) and the five config UI assets (`config-ui/routes.rs:17-21`, nested at `app/lib.rs:459`). `app/loopback-tests.rs:4` concedes "what the registry does not enumerate".
  - `crates/gateway/README.md:73` makes "Is the addition named as an extension in the spec?" a required question for every route or wire change. No spec exists to answer it.
- Goals:
  - One OpenAPI 3.1 document for the public surface, generated from the code wherever the code can generate it, committed, and regenerated by one bless command.
  - Every served route is either in the document with exactly its methods, or on a named exclusion list in code with a reason. A test enforces this.
  - The committed document always equals a fresh generation. A test enforces this and names what changed.
  - The document validates as OpenAPI 3.1. A test enforces this.
  - Every extension is marked as one, and every deliberately loose schema is named and justified.
  - Runtime behavior does not change, apart from the one debug-log difference named in Decision D3.
- Non-goals:
  - An AsyncAPI document for the `/v1/realtime` message protocol (deferred).
  - Serving the document from the Gateway (Decision D5).
  - Fixing the contradictions this plan reports. They are listed for a human to decide.
  - Describing upstream providers' schemas for pass-through bodies.
- Success criteria:
  - `cargo test -p gateway --lib openapi` passes in the default build. In a `--no-default-features` test build, the same filter passes and checks the feature markers.
  - `crates/gateway/app/openapi.json` exists and the drift test passes against it.
  - The coverage test enumerates every `(path, method)` the router mounts. That is 46 pairs at HEAD (Functional Specification, "Counts").
  - All gates in the Testing Plan pass, with warnings denied.
- Constraints:
  - Workspace lints deny clippy `all` and `pedantic`, plus `unwrap_used`, `expect_used`, and `allow_attributes_without_reason` (`Cargo.toml:202-220`). `missing_docs` is `warn` (`Cargo.toml:190`) and is promoted to an error by `CARGO_BUILD_WARNINGS=deny` (`.githooks/pre-push:9`).
  - Every `.rs` file stays at or under 500 lines (`crates/build-ceiling/src/lib.rs:14`).
  - Only `app/lib.rs`, `app/registry.rs`, and `app/test_support.rs` outside `admin/walled/` may spell `crate::admin::walled::` (`crates/build-xtask/src/tidy.rs:101-113`).
  - No doctests in doc comments (`crates/build-xtask/src/no_doctests.rs:1-14`).
  - `serde_json`'s `preserve_order` is banned workspace-wide (`deny.toml:63-67`), so every `serde_json::Map` is a sorted `BTreeMap`. Both the rendering determinism and the byte-equality tests in Step 3 rely on this.
  - Line endings are LF on every platform (`.gitattributes:1`), so the drift test can compare bytes on the Windows CI job (`.github/workflows/ci.yml:161`).
- Open questions: listed under Decision Record, "Open questions".

## Functional Specification

### Counts

At HEAD, `RouteInfo::open` and `RouteInfo::walled` appear in 36 constants: 16 open and 20 walled. Run `rg -n 'RouteInfo::(open|walled)\(' crates` to list them. Counted as `(path, method)` pairs, that is 17 open and 22 walled, because `/v1/cache` (`app/cache.rs:38`), `/admin/config` (`app/admin/walled/config.rs:45`), and `/admin/env` (`app/admin/walled/env_file.rs:49`) each mount two methods. The task statement's "14 open, 22 walled" matches the walled pair count but not the open count at this commit.

Seven more routes are mounted outside the registry, each with one method:

- the two STT routes, `POST /v1/audio/transcriptions` (`stt/batch.rs:98`) and `GET /v1/realtime` (`stt/realtime/route.rs:122`);
- the five config UI assets under `/config/`, all `GET` (`config-ui/routes.rs:17-21`, nested at `app/lib.rs:459`).

That makes 46 `(path, method)` pairs in a default build.

The public scope the task names, "the 10 `/v1` routes plus `/health`", is the 11 registry operations: chat, embeddings, rerank, speech, voices, models, web search, the three `/v1/cache` operations, and `/health`. The two STT routes are also public `/v1` routes. This plan proposes publishing all 13 operations on 12 paths (Decision D2).

Every `get()` route also answers `HEAD`: axum dispatches `HEAD` to the `GET` handler and strips the body (`axum-0.8.9/src/routing/method_routing.rs:55-56`, `:1157-1158`). The registry does not list `HEAD`, so this plan handles it with a named, tested exclusion rule (Decision D2).

### Error groups

Several routes share error paths. This table names each shared path once, so the inventory cells can say which ones a route uses. Every status, `type`, and `code` below comes from `classify()` (`app/error/classify.rs:51-228`) or `ProtocolError::classify()` (`protocol/error.rs:109-145`), and the envelope is `{"error": {"message", "type", "code"}}` (`app/error.rs:327-332`, `protocol/error.rs:150-155`).

| Group | Status, `type`, `code` | Raised by |
|---|---|---|
| auth | 401 `authentication_error` `unauthorized` (`app/error/classify.rs:53-57`) | `AuthedCaller` runs `check_auth` before any body extractor (`app/auth.rs:90-117`); `check_auth` refuses at `app/auth.rs:212` |
| host wall | 403 with an empty body (`loopback/host.rs:57-59`) | every route when the listener is bound to loopback (`app/lib.rs:483-489`); "No route is exempt, `/health` included" (`loopback/host.rs:35-37`) |
| json body | 400 `invalid_request_error` `malformed_request` (`app/error/classify.rs:72-76`) | `WireJson` maps every axum `Json` rejection (`app/error/wire.rs:8-36`): unparseable JSON, a `Content-Type` that is not `application/json` or `application/*+json` (`axum-0.8.9/src/json.rs:138-155`), a deserialize failure, or a body over the 2 MiB default limit (`axum-core-0.5.6/src/ext_traits/request.rs:317-319`) |
| validate | 400 `invalid_request_error` `malformed_request` | the request type's `validate()`, mapped at the call site (`app/relay.rs:86-88`, `:221-223`, `:250-252`; `app/speech.rs:62-64`) |
| routed model | 404 `invalid_request_error` `model_not_found` (`app/routing.rs:181-186`, `app/error/classify.rs:58-62`); 503 `server_error` `model_loading` with `Retry-After: 5` (`app/relay.rs:59-61`, `app/error.rs:322`, `:336-338`, `:341-352`); 503 `server_error` `model_provisioning` (`app/relay.rs:72-74`, `app/error/classify.rs:103-107`); 400 `invalid_request_error` `kind_mismatch` (`app/routing.rs:195-205`, `app/error/classify.rs:63-67`) | `resolve_routed_model` (`app/relay.rs:51-78`) and `require_kind` |
| admission | 503 `server_error` `queue_full` (`app/error/classify.rs:83-87`); 429 `rate_limit_error` `queue_rejected` (`app/error/classify.rs:88-92`) | `queue.admit` (`app/relay.rs:96`), mapped by `From<AdmitError>` (`app/error.rs:284-301`) |
| upstream | 502 `server_error` with `upstream_transport`, `upstream_connect`, `upstream_protocol`, or `upstream_error` (`protocol/error.rs:111-137`); 400 `invalid_request_error` `model_unavailable` (`protocol/error.rs:139-143`); any 4xx the upstream returned, passed through with `upstream_client_error` (`protocol/error.rs:126-131`) | `GatewayError::Protocol` (`app/error.rs:62-67`), and the reply checks that map a malformed upstream body to `upstream_protocol` (`app/relay.rs:146-148`, `:237-239`, `:266-268`) |
| speech upstream | the upstream group, except that an upstream 429 becomes 429 `rate_limit_error` `upstream_rate_limited` and an upstream 503 becomes 503 `server_error` `upstream_unavailable` | `app/speech.rs:101-110`; `app/error/classify.rs:93-102` |
| STT auth | the auth group's 401, plus 403 with an empty body for a disallowed `Origin` on `/v1/realtime` | `authorize_stt_route` (`app/auth.rs:231-242`), `gateway_realtime_origin_allowed` (`app/auth.rs:245-259`), `loopback/origin.rs:6-26` |
| STT batch | 400 `invalid_request_error` `malformed_request`; 404 `model_not_found`; 413 `file_too_large`; 500 `server_error` `transcription_error` | `stt/batch.rs:107-116` and `:292-326`, with the crate's own envelope builder (`stt/batch.rs:328-344`) |
| realtime upgrade | 400, 503, or 429, each with an empty body (`stt/realtime/route.rs:135-148`); axum's plain-text upgrade rejections 400, 405, and 426 (`axum-0.8.9/src/extract/ws.rs:449-489`, `:950-1001`) | `upgrade` (`stt/realtime/route.rs:130-154`) |
| cache | 500 `server_error` `cache_error` (`app/error/classify.rs:189-194`); 500 `server_error` `blocking_task_failed` (`app/error/classify.rs:134-138`) | `GatewayError::cache` and `blocking` (`app/error.rs:409-417`) |

### Route inventory: the public scope

| Path | Methods | Tier | Auth | Request | Response | Errors | Streaming | Standard or extension |
|---|---|---|---|---|---|---|---|---|
| `/health` | GET (`app/health.rs:10`) | open (`app/health.rs:10`) | None: the handler takes no caller (`app/health.rs:21`), and its doc says "unauthenticated" (`app/health.rs:20`) | none | 200 `{"status": "serving"}`, built with `json!` (`app/health.rs:22`) | host wall | no | Not classified: `crates/gateway/README.md:23` lists the routes with no standard and omits `/health`; the root `README.md:32` calls it the "liveness probe" without saying extension (open question O2) |
| `/v1/chat/completions` | POST (`app/relay.rs:27`) | open | `AuthedCaller` (`app/relay.rs:83`): bearer, handoff cookie, or loopback trust (`app/auth.rs:164-213`) | `ChatRequest` JSON (`protocol/wire.rs:30-44`), checked by `validate` (`protocol/wire.rs:69-98`, `:101-124`); with an emulated dialect, a non-array `tools` is refused (`app/dialect.rs:44-53`); optional `X-Runvil-Client` header (`app/relay.rs:24-25`, `:91-95`) | 200 `application/json` `ChatResponse` (`protocol/wire.rs:126-136`; `app/relay.rs:141-153`); with `stream: true`, 200 SSE of `ChatChunk` lines (`protocol/wire.rs:189-205`), an error-envelope line on failure, then `data: [DONE]` (`app/relay.rs:156-212`) | auth, json body, validate, routed model, admission, upstream, host wall | SSE when `stream` is true (`app/relay.rs:111`) | Standard: OpenAI (`crates/gateway/README.md:7`). Extension field `gateway_warning` inside the pass-through message (`protocol/wire.rs:12-15`; `app/dialect.rs:104-111`) |
| `/v1/embeddings` | POST (`app/relay.rs:28`) | open | `AuthedCaller` (`app/relay.rs:218`) | `EmbeddingRequest` (`protocol/wire/embedding.rs:17-31`), checked by `validate` (`:37-60`); `X-Runvil-Client` | 200 `EmbeddingResponse` (`protocol/wire/embedding.rs:63-75`; `app/relay.rs:240`) | auth, json body, validate, routed model, admission, upstream, host wall | no | Standard: OpenAI (`crates/gateway/README.md:7`) |
| `/v1/rerank` | POST (`app/relay.rs:29`) | open | `AuthedCaller` (`app/relay.rs:247`) | `RerankRequest` (`protocol/wire/rerank.rs:6-23`), checked by `validate` (`:29-55`); `X-Runvil-Client` | 200 `RerankResponse` (`protocol/wire/rerank.rs:58-70`; `app/relay.rs:269`) | auth, json body, validate, routed model, admission, upstream, host wall | no | Standard: the Jina and Cohere rerank shape (`crates/gateway/README.md:9`) |
| `/v1/audio/speech` | POST (`app/speech.rs:25`) | open | `AuthedCaller`, extracted before the body (`app/speech.rs:38-44`, `:58`) | `SpeechRequest` (`protocol/wire/speech.rs:68-96`), checked by `validate` (`:110-144`); the voice must be one the model offers (`app/speech.rs:67-88`); `X-Runvil-Client` | 200 audio bytes. `Content-Type` is the upstream's, or else the mapping in `speech_fallback_mime` (`app/speech.rs:192-198`, `:290-312`); `text/event-stream` when `stream_format` is `sse` (`app/speech.rs:298-300`) | auth, json body, validate (plus 400 `invalid_voice`, `app/error/classify.rs:77-81`), routed model, admission, speech upstream, host wall. A mid-stream failure aborts the body read with no new status (`app/speech.rs:171-179`) | Chunked byte stream with no `Content-Length` (`app/speech.rs:160-162`) | Standard: OpenAI (`crates/gateway/README.md:7`) |
| `/v1/audio/voices` | GET (`app/speech.rs:26`) | open | `AuthedCaller` (`app/speech.rs:327`) | none | 200 `{"voices": [{"id", "name"}]}`, built with `json!` (`app/speech.rs:337-339`) | auth, host wall | no | Extension (`crates/gateway/README.md:23`; `app/speech.rs:319-324`) |
| `/v1/models` | GET (`app/models.rs:16`) | open | `AuthedCaller` (`app/models.rs:73`) | none | 200 `CatalogModelsResponse` (`app/models.rs:26-33`), whose `data` items are the untagged `CatalogModelInfo` (`app/models.rs:35-44`): `ModelInfo` (`api-types/metadata.rs:180-201`, with `Capabilities` flattened, `:77-123`) or, in `stt` builds, `SpeechCatalogModelInfo` (`app/models.rs:61-68`) | auth, host wall | no | Standard: OpenAI (`crates/gateway/README.md:7`), with "Runvil extensions beside the OpenAI `id`" (`api-types/metadata.rs:180`); contradiction C4 |
| `/v1/tools/web_search` | POST (`app/web_search.rs:16`); `web-search` builds (`app/registry.rs:73-74`) | open | `AuthedCaller` (`app/web_search.rs:35`) | `WebSearchRequest` with `deny_unknown_fields` (`web-search/service.rs:82-111`) | 200 `WebSearchResponse` (`web-search/service.rs:113-140`) | auth, json body, 404 `invalid_request_error` `not_found` when unconfigured (`app/web_search.rs:38-41`, `app/error/classify.rs:69-71`), 400 from the service's checks (`web-search/service.rs:150-160`, `:239-269`, mapped at `app/error.rs:304-316`), upstream via Brave (`web-search/brave.rs:173-221`), host wall | no | Extension (`crates/gateway/README.md:23`) |
| `/v1/cache` | GET (`app/cache.rs:38`); `local` builds (`app/registry.rs:75-76`) | open | `AuthedCaller` (`app/cache.rs:68`) | none | 200 `Vec<CacheEntry>` (`app/cache.rs:66-75`; `local/cache.rs:89-100`) | auth, cache, host wall | no | Extension: "A Gateway extension, a local blob cache" (root `README.md:31`) |
| `/v1/cache` | POST (`app/cache.rs:38`); `local` builds | open | `AuthedCaller` (`app/cache.rs:113`) | `CacheRequest` (`app/cache.rs:77-84`) | On a hit, 200 JSON `{"path", "status": "ready"}` (`app/cache.rs:138-143`). On a miss, 200 SSE: `downloading` events, then a `ready` or `error` event (`app/cache.rs:103-110`, `:254-349`) | auth, json body, 400 for a bad `source` or `sha256` (`app/cache.rs:86-101`, `:117-122`), cache, host wall | SSE on a miss (`app/cache.rs:309-313`) | Extension (root `README.md:31`) |
| `/v1/cache/{sha256}` | DELETE (`app/cache.rs:39`); `local` builds | open | `AuthedCaller` (`app/cache.rs:170`) | Path capture `sha256` through plain axum `Path<String>` (`app/cache.rs:171`) | 200 `{"status": "deleted", "sha256"}` (`app/cache.rs:183-186`) | auth, 400 for a non-hex digest (`app/cache.rs:173-174`), 404 `cache_entry_not_found` (`app/cache.rs:180-181`, `app/error/classify.rs:195-200`), cache, host wall; open question O14 | no | Extension (root `README.md:31`) |
| `/v1/audio/transcriptions` | POST (`stt/batch.rs:98`); `stt` builds (`app/lib.rs:460-469`) | outside the registry, bearer-authed like the open tier | `check_auth` through the STT route layer (`app/auth.rs:231-237`) | `multipart/form-data` with fields `file`, `model`, `language`, `response_format`, `timestamp_granularities[]`, `temperature`, and `prompt`; other fields are ignored (`stt/batch.rs:143-224`); body cap 26 MiB (`stt/batch.rs:18-19`, `:99`); 16 kHz mono WAV only (`stt/batch.rs:234-244`) | 200 JSON, `json` or `verbose_json` (`stt/batch.rs:48-94`, `:266-290`) | STT auth, STT batch, host wall | no | Standard: OpenAI (`crates/gateway/README.md:7`) |
| `/v1/realtime` | GET (`stt/realtime/route.rs:122`); `stt` builds | outside the registry | `check_auth`, then the WebSocket `Origin` check (`app/auth.rs:231-242`) | Query `intent=transcription` and nothing else (`stt/realtime/query.rs:12-36`), plus the WebSocket upgrade headers (`axum-0.8.9/src/extract/ws.rs:449-489`) | 101 Switching Protocols (`stt/realtime/route.rs:151-153`), then Realtime events as JSON text frames (`stt/realtime/wire/server.rs:143-210`; `stt/realtime/wire/client.rs:10`) | STT auth, realtime upgrade, host wall | WebSocket | Standard: OpenAI Realtime transcription (`crates/gateway/README.md:7`). Extension: the hypothesis event, opted into through `session.include` (`crates/gateway/README.md:39-47`; `stt/realtime/wire/vocabulary.rs:15`) |

### Route inventory: outside the proposed scope

These routes are on the proposed exclusion list (Decision D2). They are inventoried so the exclusion is a reasoned choice. Statuses are traced to the error constructors visible in each handler's module and to its extractors. Errors raised inside callees in other crates are summarized by the mapping function that converts them.

Open admin tier: `AuthedCaller`, reachable from any admitted peer (`app/admin/open.rs:1-4`). `crates/gateway/README.md:23` calls the `/admin` routes "ours alone", so every row is an extension.

| Path | Methods | Request | Response | Errors | Streaming |
|---|---|---|---|---|---|
| `/admin/profiles` | GET (`app/admin/open/profiles.rs:17`) | none | 200 `ProfilesReply` (`app/admin/open/profiles.rs:37-42`, `:54-66`) | auth, host wall | no |
| `/admin/switch-profile` | POST (`app/admin/open/profiles.rs:18`) | `SwitchProfileRequest` (`:30-35`) | 200 `SwitchProfileReply` (`:44-52`) | auth, json body, 400 `switch_failed` (`app/admin/open/profiles.rs:92`, `app/error/classify.rs:123-127`), 400 `config_path_unavailable` (`app/admin.rs:24-30`), 404 `profile_not_found` (`app/admin/open/profiles.rs:104-108`), 422 or 500 config write (`app/error.rs:381-391`), 500 blocking, host wall | no |
| `/admin/status` | GET (`app/admin/open/status.rs:22`) | none | 200 `StatusReply` (`app/admin/open/status.rs:32-62`), whose `speech` field exists only in `stt` builds (`:59-61`) | auth, host wall | no |
| `/admin/progress` | GET (`app/admin/open/progress.rs:22`) | none | 200 SSE of `Progress` snapshots (`api-types/progress.rs:20`), with `: heartbeat` comment lines (`app/admin/open/progress.rs:36-49`, `:77-82`) | auth, host wall | SSE that "never terminates on its own" (`app/admin/open/progress.rs:39`) |
| `/admin/queue/cancel` | POST (`app/admin/open/queue.rs:22`) | none | 200 `CancelReply` (`app/admin/open/queue.rs:15-20`, `:35-45`) | auth, host wall | no |
| `/admin/queue/cancel-pending` | POST (`app/admin/open/queue.rs:23`) | `CancelPendingRequest` (`:47-51`) | 200 `CancelReply` (`:53-63`) | auth, json body, host wall | no |

Walled tier: mounted behind the loopback wall, which answers a non-loopback peer 403 before auth (`app/lib.rs:451-454`). Its handlers take `LoopbackCaller` (`app/auth.rs:119-156`), except `/config` and `/auth`, which take no caller (`app/admin/walled.rs:12-14`). The tier "reads secrets in plaintext, writes files, or launches processes" (`app/admin/walled.rs:1-6`). `crates/gateway/README.md:23` classifies `/admin` as extensions; it does not classify `/shutdown`, `/config`, or `/auth`.

| Path | Methods | Request | Response | Errors (beyond the wall's 403 and the host wall) |
|---|---|---|---|---|
| `/shutdown` | POST (`app/admin/walled/shutdown.rs:23`) | none | 202 (`:47`) | 401 |
| `/admin/restart` | POST (`app/admin/walled/restart.rs:19`) | none | 202 (`:48`) | 401; 409 `restart_unavailable` (`:39-41`, `app/error/classify.rs:164-168`) |
| `/admin/system` | GET (`app/admin/walled/system.rs:29`) | none | 200 `SystemSnapshot` (`:39-54`, `:157-169`) | 401; 500 blocking |
| `/admin/config` | GET, PUT (`app/admin/walled/config.rs:45`) | PUT: any JSON value (`:79`) | GET: 200, an untyped `serde_json::Value` (`:59-65`); PUT: 200 `ShadowReply` (`:29-35`) | 401; 400 json body; 400 `config_path_unavailable`; 422 `config_write_rejected` (`:100-110`); 500 config write (`app/error.rs:381-391`) |
| `/admin/config-pending` | GET (`app/admin/walled/config_pending.rs:56`) | none | 200 `PendingReply`, whose `profile` is an untyped `Value` (`:31-39`) | 401; 400 config path; 500 `pending_config_error` (`:112-137`, `app/error.rs:393-398`) |
| `/admin/config-dirty` | GET (`app/admin/walled/config_pending.rs:57`) | none | 200 `DirtyReply` (`:41-53`) | 401; 400 config path; 500 |
| `/admin/config-apply` | POST (`app/admin/walled/config_apply.rs:43`) | none | 200 `ApplyReply` (`:57-65`) | 401; 500 `apply_reload_failed`; 503 `apply_cancelled` (`:90-95`, `:152-153`) |
| `/admin/config-revert` | POST (`app/admin/walled/config_apply.rs:44`) | none | 200 `RevertReply` (`:68-72`) | 401; 503 `apply_cancelled` (`:164`); 500 `config_write_error` (`:191`) |
| `/admin/env` | GET, PUT (`app/admin/walled/env_file.rs:49`) | PUT: query `EnvPutQuery` (`:92-96`), body a string-to-string map (`:110`) | GET: 200 `EnvReply` (`:30-39`); PUT: 200 `ShadowReply` | 401; 400 json body or query; 422 (`:117`, `:144`, `:150`); 500 `env_file_error` (`:80`) |
| `/admin/cloud-models` | GET (`app/admin/walled/cloud_models.rs:42`) | none | 200 `Sheet` (`:386-397`) | 401; 503 `cloud_models_loading`; 502 `cloud_models_unavailable` (`:393-396`) |
| `/admin/cloud-models/refresh` | POST (`app/admin/walled/cloud_models.rs:43`) | none | 200 `Sheet` (`:404-407`) | 401; 502 `cloud_models_*` (`:301-350`) |
| `/admin/reveal` | POST (`app/admin/walled/reveal.rs:36`) | `RevealRequest` (`:46-53`) | 204 (`:134`) | 401; 400; 404 `reveal_path_not_found`; 500 `reveal_error` (`:101-106`) |
| `/admin/hf/search` | GET (`app/admin/walled/hf.rs:28`) | query `HfSearchQuery` (`:162-177`) | 200 upstream body with the upstream's `Content-Type` (`:89-123`) | 401; 400 (`:229-261`); upstream group |
| `/admin/hf/model/{owner}/{name}` | GET (`app/admin/walled/hf.rs:29`) | path captures | 200 upstream body (`:287-301`) | 401; 400 (`:273-279`); upstream group |
| `/admin/hf/model/{owner}/{name}/readme` | GET (`app/admin/walled/hf.rs:30-31`) | path captures | 200 `text/markdown; charset=utf-8` (`:122-155`, `:302`) | 401; 400 (`:327`); 404 for a missing README (`:302`) |
| `/admin/chat-templates` | GET (`app/admin/walled/chat_templates.rs:24`); `local` builds | none | 200, an untyped `Value` (`:63-112`) | 401; 500 `pending_config_error` (`:110`); 422 `model_info_error` (`:128`) |
| `/admin/orphans` | GET (`app/admin/walled/orphans.rs:32`); `local` builds | none | 200 `OrphansReply` (`:25-30`) | 401; 500 cache (`:73`) |
| `/admin/model-info` | GET (`app/admin/walled/model_info.rs:24`); `local` builds | query `ModelInfoQuery` (`:34-39`) | 200 `gguf::ModelInfo` (`:57`) | 401; 400 (`:68-71`); 422 `model_info_error` (`:72`) |
| `/config` | GET (`app/admin/walled/handoff.rs:33`); `config-ui` builds | none | 308 redirect to `/config/` (`:50-54`) | none beyond the walls |
| `/auth` | GET (`app/admin/walled/handoff.rs:34`); `config-ui` builds | query `key` (`:66-70`) through plain axum `Query` (`:82`) | 302 to `/config/` with `Set-Cookie` and `Cache-Control: no-store` (`:97-104`) | 401 (`:87`); a malformed query is axum's plain-text 400 (`:82`) |
| `/config/`, `/config/app.js`, `/config/app.css`, `/config/icons/runvil-icon.png`, `/config/icons/runvil-icon@2x.png` | GET (`config-ui/routes.rs:17-21`); `config-ui` builds | none | 200 static asset with a fixed content type (`config-ui/routes.rs:25-50`) | Peer-wall 403 only; no bearer auth (`config-ui/routes.rs:22`) |

### What the document states, route-independently

- **Authentication.** Two security schemes, both taken from `check_auth` (`app/auth.rs:164-213`). `bearer` is HTTP bearer, with the case-sensitive `Bearer ` prefix the code strips (`app/auth.rs:187-191`). `sessionCookie` is an API key in the cookie `runvil-gateway-session` (`app/auth/primitives.rs:27`), accepted only with `Sec-Fetch-Site: same-origin` or `none` (`app/auth/primitives.rs:71-85`). OpenAPI cannot express loopback trust: with `[server] trust_loopback` on, which is the default (`config/config.rs:323-327`), a loopback peer that sends no `Authorization` header and passes the Fetch Metadata rule is admitted (`app/auth.rs:205-210`; `app/auth/primitives.rs:88-102`). The document states this in prose and marks the affected operations with `x-runvil-loopback-trust: true`. `/health` has `security: []`.
- **Error envelope.** One `ErrorEnvelope` schema describes all three builders (`app/error.rs:327-332`, `protocol/error.rs:150-155`, `stt/batch.rs:328-344`). The host wall, the walled peer wall, the realtime `Origin` refusal, and the realtime upgrade refusals answer with empty or plain-text bodies, and the document says so for each.
- **Streaming.** OpenAPI cannot describe event streams fully. Each SSE response is `text/event-stream`, with a description taken from the emitting function's doc comment (`app/relay.rs:156-169`, `app/cache.rs:254-264`) and the per-line JSON schemas listed by name: `ChatChunk`, `ErrorEnvelope`, and the cache event shapes from Step 3.
- **Pass-through bodies.** Every `serde_json::Value` field and every flattened `rest` map is a deliberately loose schema. Each is named in the allowlist under Decision D13, and its description comes from the field's own doc comment, such as "passed through to the backend verbatim" (`protocol/wire.rs:35-36`).
- Acceptance criteria:
  - Each of the 13 in-scope operations appears with the auth, request, responses, and errors in its inventory row, and every element in the document traces to a cited source.
  - Every other `(path, method)` the router mounts is on the exclusion list with its reason.
  - Every extension in the public scope carries the marking convention from Decision D6.

</product-contract>
<implementation-contract>

## Technical Design

### The tooling decision

All facts below were checked on 2026-11-09 (PT) through the crates.io API (`https://crates.io/api/v1/crates/<name>` and `.../<name>/<version>/dependencies`) and in each crate's published source.

| | utoipa 6.0.0 (+ utoipa-axum 0.3.0) | aide 0.15.1 / 0.16.0-alpha.4 | schemars 1.2.3 plus the registry (recommended) |
|---|---|---|---|
| Release | 6.0.0 published 2026-09-22; utoipa-gen 6.0.1 on 2026-09-23 | 0.15.1 (stable) published 2025-08-19; 0.16.0-alpha.4 published 2026-04-14 | 1.2.3 published 2026-10-12, already in the lock (`Cargo.lock:3857-3860`) |
| OpenAPI 3.1 | Default `OpenApiVersion::Version31` serializes as `3.1.0` (`utoipa-6.0.0/src/openapi.rs:482-486`) | Writes `3.1.0` (`aide-0.16.0-alpha.4/src/openapi/openapi.rs:105-110`) | Draft 2020-12 output (`schemars-1.2.3/src/generate.rs:108-117`); the OAS 3.1 Schema Object dialect is built on draft 2020-12 (its `$schema`, in `dialect/2024-11-10`). The prototype below validated such a document against the official OAS 3.1 schema |
| axum 0.8 | utoipa-axum 0.3.0 requires `axum ^0.8.4` | `axum ^0.8.1` behind the `axum` feature | Needs no axum integration: paths come from `RouteInfo` (`app/registry.rs:32-43`) |
| schemars 1 | Not used. utoipa 6.0.0 depends only on `indexmap`, `serde`, `serde_json`, and `utoipa-gen` (plus optional `yaml_serde`), so its `ToSchema` would be a second schema system beside `sheet.schema.json` | 0.15.1 requires `schemars ^0.9`, a different major version whose `JsonSchema` trait the workspace's types do not implement. Only the alpha requires `schemars ^1.0.4` | Native: `schemars = "1"` (`Cargo.toml:134`), used by `gateway-api-types` (`crates/gateway-api-types/Cargo.toml:10`) |
| What it asks of the code | `#[derive(ToSchema)]` on every wire type across six crates, including `gateway-api-types`, whose crate docs say it depends only on `serde`, `time`, and `schemars` (`api-types/lib.rs:7-9`). `#[utoipa::path(...)]` on every handler, restating method, path, and statuses by hand; `path` accepts an expression (`utoipa-gen-6.0.1/src/path.rs:46`, `:136`). Either an `OpenApi` derive listing the paths, or `OpenApiRouter` replacing `Router` | `ApiRouter` replaces `Router` in every area's `routes()` (`aide-0.16.0-alpha.4/src/axum/mod.rs:213`, `:301`, `:405`). `OperationInput` impls for `WireJson`, `AuthedCaller`, and `LoopbackCaller` (`aide-0.16.0-alpha.4/src/operation.rs:44`, `:141`). Per-route status transforms, because the handlers return `Response` or `GatewayError` | `#[derive(JsonSchema)]` on the in-scope wire types in five crates and the app. Operation docs are declared in one test-only module. No handler or router change |
| Accuracy | A `#[serde(default)]` field is never required (`utoipa-gen-6.0.1/src/component.rs:56-60`). That is wrong for response fields the code always emits, such as `Capabilities.images` (`api-types/metadata.rs:95-98`) | Uses schemars, so the alpha would match the third column | Separate serialize and deserialize contracts (`schemars-1.2.3/src/generate.rs:174-186`; `schemars_derive-1.2.3/src/schema_exprs.rs:730-740`): `images` is required in a response, and `stream` is optional in a request |
| Architecture checks | Macro output on handlers meets `missing_docs`, `unreachable_pub`, and pedantic clippy. That interaction was not tested in a spike | Router replacement in every area module | Generator modules count against the 500-line ceiling. The generator must not spell `crate::admin::walled::`, and does not need to |
| New dependencies | utoipa, utoipa-gen, utoipa-axum, pastey, tower-layer, tower-service as normal dependencies compiled into the release binary | aide, aide-macros (+ serde_qs) as normal dependencies; the alpha for schemars 1 | No new runtime crate. Dev-only `jsonschema` adds 27 lock entries, measured in a scratch worktree (Project Survey) |
| License | MIT OR Apache-2.0 | MIT OR Apache-2.0 | schemars MIT; jsonschema MIT. `cargo deny check bans licenses sources` passes with it added (measured) |
| Coverage | The `path` attribute duplicates `RouteInfo.path` unless written as `path = CONST.path` | Routes are documented where they are mounted; the registry stays a second list | The registry is the source. Coverage holds by construction and is checked by a test |

Recommendation: the third column, as Decision D1.

### Prototype evidence

A standalone crate outside the repository (`/tmp/proto`, not committed) derived schemas with schemars 1.2.3, using `SchemaSettings::draft2020_12()` with `definitions_path = "/components/schemas"` and `meta_schema = None` (`schemars-1.2.3/src/generate.rs:37`, `:41`), one generator per contract. It wrapped them in an OpenAPI 3.1.0 envelope and validated the result with jsonschema 0.61.0 (`default-features = false`) against `schema-base/2026-08-03`, registered offline together with `schema/2026-08-03`, `dialect/2024-11-10`, and `meta/2024-11-10`. Observed results:

- `$ref` values render as `#/components/schemas/<Name>`.
- A flattened `Map<String, Value>` renders as `"additionalProperties": true`, and `Vec<Value>` renders as `"items": true` (`schemars-1.2.3/src/json_schema_impls/serdejson.rs:8-20`).
- Serialize contract: a `#[serde(default)] images: bool` is required, and a `skip_serializing_if` field is not.
- `#[schemars(length(min = 1, max = MAX_CHARS))]` and `range(min = MIN_SPEED, max = MAX_SPEED)` accept constant paths (`schemars_derive-1.2.3/src/attr/validation.rs:162-170`) and emit `minLength`, `maxLength`, `minimum`, and `maximum` from them.
- A `#[serde(default)]` enum field gets `"default": "mp3"`, computed from `Default` (`schemars_derive-1.2.3/src/schema_exprs.rs:712-716`).
- One type used under both contracts produced identical schemas, so merging the two generators' definitions worked. The merge fails loudly if they ever differ.
- The document validated. Negative controls behaved as expected: a response code `abc` was rejected by `schema`; `"required": "model"` inside a component was rejected by `schema-base` but not by `schema`, which is why the validity test uses `schema-base`. An operation without `responses` passes, because OAS 3.1 made `responses` optional, so the structural checks enforce it themselves.

### Document assembly (Decision D1 in detail)

The generator is test-only code in the `gateway` crate (`#[cfg(test)] mod openapi;` in `app/lib.rs`). It needs crate-private items, namely the registry, `CatalogModelsResponse`, and `CacheRequest`, and nothing in the release binary needs it.

- `app/openapi.rs`: the entry point, `document() -> Result<serde_json::Value, Vec<GenerationError>>`. It walks `registry::all()`, pairs each in-scope `(RouteInfo, Method)` with its `Operation`, checks the exclusion list, and assembles `paths`, `components.schemas`, `components.securitySchemes`, and `tags`.
- `app/openapi/operations.rs`: one `Operation` per in-scope `(path, method)`. It names the handler (file and fn), the request body schema function and media type, the success responses (status, media type, schema function), the error groups, the standard or extension marker, and the cfg feature.
- `app/openapi/errors.rs`: the error groups from the Functional Specification. Each entry builds a sample `GatewayError` and reads its status, its `Retry-After` header, and the envelope's `type` and `code` from `into_response()`, the same path the routes use (`app/error.rs:341-352`), so `classify()` keeps its `pub(super)` visibility. Each response description is the variant's doc comment, read from source.
- `app/openapi/source.rs`: reads sources with `syn`, which is already a workspace dependency (`Cargo.toml:145`). It extracts handler and variant doc comments the way schemars joins type docs (`schemars_derive-1.2.3/src/attr/doc.rs`). It confirms that each area's `routes()` mounts the named handler on `<CONST>.path` with the named method, the form `app/registry.rs:4-6` already mandates. It reads the `#[cfg(feature = ...)]` attributes on the `registry::all()` statements for the `local` marker (Decision D7), and it collects the `match` arm literals of `parse_form` (`stt/batch.rs:157-215`) for the multipart field check.
- `app/openapi/render.rs`: canonical rendering, copied from `crates/gateway-api-types/tests/sheet_schema.rs:39-68` (sorted keys, two-space indent, trailing newline), plus a JSON-pointer diff for the drift message.
- Components: two `SchemaGenerator`s, requests under `for_deserialize()` and responses under `for_serialize()`. Their definitions merge; equal schemas under one name collapse, and differing ones fail with `ContractConflict { name }`. schemars silently renames a second type with the same name to `Name2` (`schemars-1.2.3/src/generate.rs:336-348`), so the generator fails with `SchemaNameCollision` when a definition `NameN` sits beside `Name`.
- `GenerationError` variants name the item and the reason: `HandlerNotFound`, `HandlerUndocumented`, `MountMismatch`, `SchemaNameCollision`, `ContractConflict`, `LooseSchemaNotAllowlisted`, `StaleAllowlistEntry`, `UnresolvedRef`, `UnreferencedComponent`, `DuplicateOperationId`, `PathParameterUndeclared`, `UnsupportedPathTemplate`, and `BlessInPartialBuild`. Generation returns every error it found, not the first.
- Descriptions are verbatim doc comments (Decision D10). A handler with no doc comment fails generation unless it is on the `UNDOCUMENTED` list in code. Today that list holds the STT `handler` (`stt/batch.rs:103`) and `upgrade` (`stt/realtime/route.rs:130`), and their description reads "Unspecified: the handler has no doc comment" plus the `x-runvil-source` path.
- `info`: `title` "Runvil Gateway"; `version` from `CARGO_PKG_VERSION`, which comes from `Cargo.toml:15`; `description` from the first paragraph of `crates/gateway/README.md:3`; `license` `{"name": "BSL-1.0", "identifier": "BSL-1.0"}` from `Cargo.toml:20`. There is no `servers` entry: the bind address is operator configuration (`config/config.rs:318-319`).
- `operationId` is `<method>_<path>` with `/` and braces replaced by `_` (Decision D20), unique by construction and checked.
- Tags are the area module names, `relay`, `speech`, `models`, `health`, `web_search`, and `cache`, plus `stt`. Each tag's description is its module's first doc paragraph, such as `app/relay.rs:1-3`.

### Scope and exclusions (Decision D2)

The exclusion list is a `const EXCLUDED: &[Exclusion]` in `app/openapi.rs`. Each entry holds a path, methods, a reason enum, a sentence citing the code, and an optional cfg feature. Reasons:

- `WalledTier`, for the 22 walled pairs. The tier is loopback-only and "reads secrets in plaintext, writes files, or launches processes" (`app/admin/walled.rs:1-6`). Its config documents are untyped `Value`s (`app/admin/walled/config.rs:64`, `app/admin/walled/config_pending.rs:36`, `app/admin/walled/chat_templates.rs:66`). Its consumer is the embedded settings page (`crates/gateway/config-ui/README.md:3`).
- `ConfigUiAsset`, for the five static assets: they are a web page, not an API (`config-ui/routes.rs:9-14`).
- `OpenAdminDeferred`, for the six open admin pairs, pending Decision D2's follow-up step.
- `ImplicitHead`, a rule rather than an entry: every `GET` path also answers `HEAD` (`axum-0.8.9/src/routing/method_routing.rs:1157-1158`). The probe test checks that `HEAD` returns the same status as `GET` with an empty body on every `GET` route, except `/v1/realtime`. There the upgrade extractor refuses any non-`GET` method with 405 (`axum-0.8.9/src/extract/ws.rs:451-453`), so `HEAD` answers 405 while a plain `GET` without upgrade headers answers 400 (`axum-0.8.9/src/extract/ws.rs:455-457`). Both outcomes are read from the axum source; the test pins them and the exception by name.

### The extension marking convention (Decision D6)

OpenAPI reserves `x-` keys for extensions, so every Gateway marker is `x-runvil-*`:

- `x-runvil-standard`, on an operation that follows a standard. The value is one of `openai` (`crates/gateway/README.md:7`), `jina-cohere-rerank` (`crates/gateway/README.md:9`), or `openai-realtime-transcription` (`crates/gateway/README.md:7`).
- `x-runvil-extension: true`, on an operation with no standard behind it (`crates/gateway/README.md:23`), and on every extension parameter, header, or property inside a standard operation. Property-level markers are written beside the field as `#[schemars(extend("x-runvil-extension" = true))]`.
- `x-runvil-opt-in`: where the code shows an opt-in mechanism, a sentence naming it. Examples: the `X-Runvil-Client` header the client chooses to send (`routing/queue-client.rs:3-9`), and the `session.include` value (`stt/realtime/wire/vocabulary.rs:15`). Elsewhere `false`, so the README's opt-in property (`crates/gateway/README.md:21`) is auditable field by field.
- `x-runvil-extension-fields`, on an operation whose extension lives inside a pass-through `Value` that has no typed property to mark. Today that is `choices[].message.gateway_warning` (`app/dialect.rs:111`), whose description comes from `protocol/wire.rs:12-15`.
- `x-runvil-feature`: the cfg feature that compiles the element in (`local`, `web-search`, or `stt`).
- `x-runvil-pass-through: true`: added by the generator at each allowlisted loose schema (Decision D13).
- `x-runvil-loopback-trust: true`: on operations whose auth admits loopback trust.
- `x-runvil-websocket: true`: on `GET /v1/realtime`.
- `x-runvil-source`: `<file>#<item>` for the handler, with no line number, so unrelated edits do not churn the artifact.

The extension classification of each public field is in the Decision Record (Decision D6, the classification table) and awaits approval. Where the code does not settle a classification, it is an open question.

### Feature-gated shapes (Decision D7)

`crates/gateway/app/openapi.json` describes the default build, which has every route-bearing feature: `default = ["local", "web-search", "config-ui", "stt"]` (`crates/gateway/app/Cargo.toml:130`). `test-fixtures` adds no routes; it only test-scales the speech relay bounds (`app/speech.rs:114-151`) and STT policies (`stt/realtime/route.rs:39-114`). Every element compiled in by a feature carries `x-runvil-feature`. A client of a `--no-default-features` build reads the document with every marked element removed.

The markers are checked in two ways, because no test build can turn `local` off. The crate's self dev-dependency enables `test-fixtures` (`crates/gateway/app/Cargo.toml:155`), which enables `local` (`crates/gateway/app/Cargo.toml:145`). `cargo tree -p gateway --no-default-features -e features,dev -i gateway` shows `local` enabled through `test-fixtures` in that build.

- In a `cargo test -p gateway --no-default-features` build, which has `local` and `test-fixtures` only and was measured to compile in 64 s, the feature-shape test checks one equation. The generated document must equal the committed one with every `web-search`, `stt`, and `config-ui` element removed. That proves those markers in both directions: nothing marked survives, and nothing unmarked disappears. The removed elements include `CatalogModelInfo`'s `Speech` branch (`app/models.rs:42-43`).
- The `local` markers are checked from source. The generator reads the `#[cfg(feature = "local")]` attribute on `routes.extend_from_slice(cache::ROUTES)` (`app/registry.rs:75-76`) and requires every cache operation to carry `x-runvil-feature: local`.
- CI gains one step in the `test` job: `cargo nextest run --locked -p gateway --no-default-features -E 'test(/openapi/)'`.

### Artifact, rendering, and bless (Decision D4)

- Path: `crates/gateway/app/openapi.json`, beside its crate's manifest, as `crates/gateway-api-types/sheet.schema.json` is.
- Rendering: sorted keys, two-space indent, trailing newline, no timestamps, no machine paths. Source references are repository-relative. `info.version` changes only when the workspace version changes.
- Regenerate: `BLESS_OPENAPI=1 cargo test -p gateway --lib openapi`, held in one `REGENERATE` constant as `crates/gateway-api-types/tests/sheet_schema.rs:23` holds its command. Blessing refuses in a build missing any of `local`, `web-search`, `config-ui`, or `stt`, and names the missing ones.
- The drift failure lists the JSON pointers added, removed, and changed (at most 50, then a count), and ends with the regenerate command.

### Serving the document (Decision D5)

Not in this task. Serving `GET /openapi.json` would add a route, so it would need a tier (open), auth (`AuthedCaller`, since the root `README.md:36` says every route except `/health` is authenticated by default), and `x-runvil-extension: true`. It would also move the generator out of `#[cfg(test)]`, or embed the committed artifact with `include_str!`, which is simpler and drift-tested by construction. It also overlaps the capability endpoint `crates/gateway/README.md:27-35` describes, which "no such endpoint exists yet" (`crates/gateway/README.md:29`). That is a design question for the reviewer.

### File and public API changes (Phase 2, all steps)

- `crates/gateway/stt/api`: export the two path constants and bind them in `routes()` (`stt/batch.rs:98`, `stt/realtime/route.rs:122`). Export one `pub` function returning the transcription response schema, so the private batch types stay private. Add `schemars` and derive `JsonSchema` on `JsonTranscription`, `TranscriptionSegment`, `VerboseJsonTranscription`, and `TranscriptionResponse` (`stt/batch.rs:48-94`).
- `crates/gateway/config-ui`: export the asset path constants and bind them in `routes()` (`config-ui/routes.rs:17-21`).
- `crates/gateway/protocol`: add `schemars` and derive `JsonSchema` on the wire types in `protocol/wire.rs`, `protocol/wire/embedding.rs`, `protocol/wire/rerank.rs`, and `protocol/wire/speech.rs`. `SpeechRequest.input` gets `length(min = 1, max = MAX_SPEECH_INPUT_CHARS)` and `speed` gets `range(min = MIN_SPEECH_SPEED, max = MAX_SPEECH_SPEED)` (`protocol/wire/speech.rs:8-14`).
- `crates/gateway-api-types`: derive `JsonSchema` on `ModelInfo`, `Capabilities`, and `ThinkingMode` (`api-types/metadata.rs:22`, `:87`, `:182`). None is referenced by `Sheet`, so `sheet.schema.json` must not change, and its drift test proves that.
- `crates/gateway/web-search`: add `schemars` and derive `JsonSchema` on `WebSearchRequest`, `WebSearchResponse`, and `SearchResult` (`web-search/service.rs:82-140`).
- `crates/gateway/local`: add `schemars` and derive `JsonSchema` on `CacheEntry` (`local/cache.rs:89-100`).
- `crates/gateway/app`:
  - `registry.rs` and `lib.rs` add the seven external routes and correct their docs.
  - Derive `JsonSchema` on the four private types `CatalogModelsResponse`, `CatalogModelInfo`, `SpeechCatalogModelInfo` (`app/models.rs:26-68`), and `CacheRequest` (`app/cache.rs:77-84`), and widen them to `pub(crate)` (Decision D16).
  - Add the typed replies from Step 3, the test-only `openapi` module, and `openapi.json`.
  - Add dev-dependencies `jsonschema` (`default-features = false`), `syn`, and `proc-macro2`.
  - Vendor `tests/fixtures/oas-3.1/` (four JSON files plus the Apache-2.0 notice).
- Root `Cargo.toml`: add `jsonschema = { version = "0.61", default-features = false }` to `[workspace.dependencies]` with a comment.
- `.github/workflows/ci.yml`: the one step from Decision D7.
- Docs: a "The committed OpenAPI description" section in `crates/gateway/app/README.md`, mirroring `crates/gateway-api-types/README.md:5-13`, and `crates/gateway/README.md:75` updated to point at the artifact.
- No public API change to the `gateway` library. The new `pub` items are in `gateway-stt` (two constants and one function) and `gateway-config-ui` (one constant list).

</implementation-contract>
<verification-contract>

## Testing Plan

Every check below fails with a message that names the exact route, pointer, type, or feature that is wrong.

- **Coverage** (`app/openapi-tests.rs`). It takes the set of `(path, method)` from `registry::all()` after Step 1 and compares it with the generated paths plus `EXCLUDED`. Failures name each missing pair ("`POST /v1/x` is mounted (registry) but neither documented nor excluded"), each extra pair ("documented but not mounted"), each method mismatch per path, and each stale exclusion. It also rejects axum wildcard templates (`{*`), which OpenAPI cannot express.
- **Mount binding.** For each operation, `syn` confirms that the area's `routes()` mounts the named handler with the named method on that `RouteInfo`'s path. Failures name the path and both handlers.
- **Drift.** It compares the committed `openapi.json` byte for byte with a fresh render, prints the JSON-pointer diff, and ends with `BLESS_OPENAPI=1 cargo test -p gateway --lib openapi`. A missing file fails with the same command. This mirrors `crates/gateway-api-types/tests/sheet_schema.rs:112-165`.
- **Validity.**
  - jsonschema 0.61 (dev-only, `default-features = false`, so it never fetches) validates against the vendored `schema-base/2026-08-03`, with `schema/2026-08-03`, `dialect/2024-11-10`, and `meta/2024-11-10` registered offline. The draft 2020-12 meta-schemas are bundled in `referencing-0.61.0/src/meta.rs:48-79`.
  - Structural checks the meta-schema does not make: every operation has `responses`; every `$ref` resolves; every component is referenced; `operationId`s are unique; every path template parameter has a `parameters` entry, and the reverse.
  - Each error is printed with its instance pointer.
- **Loose schemas.** The generator walks the document and fails on any `true`, `{}`, or `"additionalProperties": true` that is not at an allowlisted pointer. It also fails on any allowlist entry it did not find.
- **Feature shapes.** The reduced-build equation and the `local` source check from Decision D7.
- **Multipart fields.** The `match` arm literals in `parse_form` (`stt/batch.rs:157-215`) must equal the documented form properties, naming the extras on either side. Unknown fields are silently ignored (`stt/batch.rs:214`), so this is the only check that sees a new field.
- **Conformance** (Step 7). It drives each in-scope route through `build_router` with the existing fixtures (`app/test_support.rs:30`, `:104`, `:329`) and the integration backends (`crates/gateway/app/tests/it/support/backends.rs`), then validates each body against the document's schema for that path, method, status, and media type, one line at a time for SSE. It covers every `GatewayError` variant the error groups list, built directly and validated against `ErrorEnvelope`, plus the STT batch builder through the router. Each case names its route and status. A status the document lists but no case provokes is printed as unexercised, so the gap is visible rather than silent.
- **Implicit HEAD.** A probe test over every registry `GET` route, with the named `/v1/realtime` exception.
- **No behavior change.**
  - The existing suite passes unmodified. Baseline at `9369a7d` on Linux: `cargo nextest run --locked --workspace --all-features` ran 1940 tests, all passed, 54 skipped.
  - The registry sweeps (`app/registry-tests.rs:92-172`) cover the seven added routes without edits.
  - Step 3's byte-equality tests compare each typed reply with the `json!` literal it replaces.
  - `sheet.schema.json`'s drift test passes untouched after Step 2.
- **Exit criteria** (every step, with warnings denied):
  - `cargo fmt --all --check`
  - `CARGO_BUILD_WARNINGS=deny cargo clippy --locked --workspace --all-targets --all-features`, which is CI's command (`.github/workflows/ci.yml:59`) with the pre-push hook's warning setting (`.githooks/pre-push:9`)
  - `cargo nextest run --locked --workspace --all-features`, after `npm ci --prefix crates/gateway/config-ui/ui`
  - `cargo test -p build-xtask`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --all-features`
  - `cargo check --locked -p gateway --no-default-features`
  - From Step 6: `cargo nextest run --locked -p gateway --no-default-features -E 'test(/openapi/)'`
  - `cargo deny check bans licenses sources`. `advisories` already fails at baseline (Decision Record, "Notes").

</verification-contract>
<decision-record>

## Decision Record

Every decision below is a recommendation awaiting approval.

- Decisions:
  - **D1. Assemble the document from schemars 1 schemas and the `RouteInfo` registry, with no OpenAPI crate.**
    - Rationale: it needs no new runtime dependency and no router or handler change, and it keeps one schema system beside `sheet.schema.json`. It describes responses with the serialize contract, which utoipa cannot, and it treats the registry as the route source (Technical Design, "The tooling decision").
  - **D2. Scope: the 11 registry operations named in the task plus the two STT routes, 13 operations on 12 paths.**
    - Excluded with reasons: the 22 walled pairs, the 5 config UI assets, and the 6 open admin pairs; implicit `HEAD` is handled by a tested rule.
    - Follow-up, once Phase 2 lands: publish the open admin tier in the same document under tag `admin`, marked as extensions. Its types are typed apart from the `stt`-gated `speech` field (`app/admin/open/status.rs:59-61`), and `Progress` (`api-types/progress.rs:20`) would need `JsonSchema`.
    - The walled tier stays excluded. It could be a separate internal document later, if the settings page ever needs one.
  - **D3. Add the seven external mounts to `registry::all()`.**
    - The STT and config UI crates export path constants and bind them in their own `routes()`, the "bind the constant, never a literal" rule of `app/registry.rs:4-6`.
    - This makes `app/registry.rs:8-11` and `app/lib.rs:32-36` true, and the existing sweeps start covering these routes.
    - The one runtime-observable change in this plan: seven more `route mounted` lines at debug level (`app/lib.rs:470-480`).
    - It touches the STT and config UI crate boundaries, which the decentralization plan deliberately left alone (`vibe/2026-09/2026-09-20-3-gateway-route-decentralization.md:155`).
    - Alternative: a separate `registry::external()` used only by tests, with no log change. The two doc comments would then be reworded instead.
  - **D4. Artifact `crates/gateway/app/openapi.json`, regenerated with `BLESS_OPENAPI=1 cargo test -p gateway --lib openapi`, with `info.version` from `CARGO_PKG_VERSION`.**
    - A workspace version bump requires a rebless, and CI catches a missed one.
    - JSON rather than YAML, to match the sheet artifact and avoid a YAML dependency.
  - **D5. Do not serve the document in this task.** The questions it would raise are in Technical Design.
  - **D6. The `x-runvil-*` marking convention.** The proposed classification of public extension elements:

    | Element | Proposed | Evidence |
    |---|---|---|
    | `X-Runvil-Client` request header | extension, opt-in | `app/relay.rs:24-25`; `routing/queue-client.rs:3-9` |
    | `ModelInfo` fields other than `id` (`kind`, `description`, `context`, `thinking`, flattened `Capabilities`) | extension, not opt-in | "Runvil extensions beside the OpenAI `id`" (`api-types/metadata.rs:180`); `object` is open question O3 |
    | `SpeechCatalogModelInfo.kind` | extension, not opt-in | Same catalog shape (`app/models.rs:61-68`); no doc comment (M2) |
    | `choices[].message.gateway_warning` | extension, not opt-in | "a gateway-specific extension on the OpenAI response shape" (`protocol/wire.rs:12-15`) |
    | Realtime `conversation.item.input_audio_transcription.hypothesis` event | extension, opt-in through `session.include` | `crates/gateway/README.md:39-47`; `stt/realtime/wire/vocabulary.rs:15`; `stt/realtime/wire/server.rs:193` |
    | `/v1/audio/voices`, `/v1/tools/web_search` | extension routes | `crates/gateway/README.md:23` |
    | `/v1/cache`, `/v1/cache/{sha256}` | extension routes | "A Gateway extension, a local blob cache" (root `README.md:31`) |
    | `/health` | extension route | Listed as the "liveness probe" (root `README.md:32`), not classified (O2) |
  - **D7. One artifact for the default build, with `x-runvil-feature` markers checked by the reduced build and by source, and one new CI step.**
    - Two artifacts were rejected: no test build can produce the true `--no-default-features` shape, because `local` is always on in tests.
  - **D8. `/v1/realtime` as a documented stub operation.**
    - It has the `intent` query parameter, the `101` response, and every pre-upgrade refusal. The description points at `stt/realtime/wire/client.rs` and `stt/realtime/wire/server.rs:143-210`, and the operation carries `x-runvil-websocket: true`.
    - AsyncAPI is deferred.
  - **D9. Type the in-scope `json!` replies whose fields cannot fail to serialize, so the schema comes from code.**
    - Typed: `app/health.rs:22`, `app/speech.rs:337-339`, `app/cache.rs:183-186` (delete), and `:324-328` (the `downloading` event), whose fields are strings and numbers.
    - Kept as `json!`: `app/cache.rs:139-142` (the hit reply) and the `ready` and `error` events at `:335-347`, which interpolate a `PathBuf` or a `String`. The hit reply and the `ready` event carry the `PathBuf`, and `json!` unwraps its serialization (open question O16). The `error` event is kept with them, so the terminal events share one form. They get schema-only types, pinned by conformance.
    - Fields are declared in sorted key order. `json!` builds a `serde_json::Map`, which is sorted, while a struct serializes in declaration order, so this keeps the bytes identical, and a test pins each one.
    - Alternative: keep the literals and hand-write their schemas, guarded only by conformance tests.
  - **D10. Descriptions are verbatim doc comments.** This covers types through schemars, and handlers and error variants through `syn`. A missing handler doc fails generation unless it is listed in `UNDOCUMENTED`, which renders "Unspecified". Rustdoc link brackets stay as written.
  - **D11. Error responses come from error groups declared per operation.**
    - Status, `type`, `code`, and `Retry-After` are read from `into_response()` on a sample of each variant. Descriptions are the variant docs.
    - Codes are listed in `x-runvil-error-codes` and in prose, not as an `enum` constraint, because a missed code would then make the document reject a real response.
    - Upstream pass-through 4xx use the `4XX` range key.
  - **D12. Security schemes `bearer` and `sessionCookie`, with loopback trust in prose plus `x-runvil-loopback-trust: true`, and `/health` as `security: []`.**
    - The cookie is real behavior on every `AuthedCaller` route (`app/auth.rs:196-203`), so leaving it out would understate what the routes accept.
  - **D13. The loose-schema allowlist.** Each entry is a JSON pointer with a reason and a citation:
    - `ChatRequest.messages[]` (`protocol/wire.rs:35-36`) and its `rest` (`:41-43`)
    - `ChatResponse.choices[]` and `rest` (`protocol/wire.rs:131-135`)
    - `ChatChunk.rest` (`protocol/wire.rs:201-204`)
    - `ChatChunkChoice.delta` and `rest` (`protocol/wire.rs:234-240`)
    - `EmbeddingRequest.rest` (`protocol/wire/embedding.rs:28-30`)
    - `EmbeddingResponse.data[]` and `rest` (`protocol/wire/embedding.rs:69-74`)
    - `RerankRequest.rest` (`protocol/wire/rerank.rs:20-22`)
    - `RerankResponse.results[]` and `rest` (`protocol/wire/rerank.rs:64-69`)
    - `SpeechRequest.rest` (`protocol/wire/speech.rs:93-95`)
    - `VerboseJsonTranscription.words[]` (`stt/batch.rs:81-83`)
  - **D14. Schema constraints only where the code's own constants can feed a schemars attribute**, which today means the speech `input` length and `speed` range. Every other check stays in prose, taken from the doc of the function that makes it. Examples: `ChatRequest::validate` (`protocol/wire.rs:69-80`), the web search knob checks (`web-search/service.rs:229-238`), and `ClientId`'s bucketing (`routing/queue-client.rs:3-9`), which never rejects, so the header schema stays a plain string.
  - **D15. Validate with jsonschema 0.61, dev-only, against the vendored OAS 3.1 `schema-base/2026-08-03`.** That is the newest iteration listed at `https://spec.openapis.org/oas/` on 2026-11-09 (PT). The schema files are Apache-2.0 (the `LICENSE` of `https://github.com/OAI/OpenAPI-Specification`), so the vendored copy carries the notice.
  - **D16. Widen four private app types to `pub(crate)`.** They are `CatalogModelsResponse`, `CatalogModelInfo`, `SpeechCatalogModelInfo`, and `CacheRequest`. This lets the central operation table name them. The rejected alternative was an `<area>-openapi.rs` child module per area, which would push `relay`, `speech`, `models`, and `cache` past the three-file rule into directories (`app/lib.rs:47-48`).
  - **D17. Dependencies.**
    - `schemars` becomes a normal dependency of `gateway-protocol`, `gateway-web-search`, `gateway-local`, `gateway-stt`, and `gateway`. It is already compiled into the gateway through `gateway-api-types`, so no new crate is built.
    - `jsonschema`, `syn`, and `proc-macro2` become dev-dependencies of `gateway`. `syn` and `proc-macro2` are already workspace pins (`Cargo.toml:117`, `:145`).
  - **D18. A new `BLESS_OPENAPI` variable, separate from `BLESS_SCHEMA`.** That way one command never rewrites both artifacts.
  - **D19. No hand-written `const` for fixed literals** such as `"list"`, `"model"`, `"serving"`, and `"transcription"`. Their doc comments already say "Always `"list"`" (`app/models.rs:29`) and "Always `"model"`" (`api-types/metadata.rs:185`). The `/health` example is produced by calling the handler. Option: add `extend("const" = ...)` beside each literal, pinned by conformance.
  - **D20. Mechanical `operationId`s** such as `post_v1_chat_completions`, because two handlers are named `handler` and `upgrade` (`stt/batch.rs:103`, `stt/realtime/route.rs:130`).
  - **D21. `x-runvil-source` on operations**, as `<file>#<item>` without line numbers.
- Rejected alternatives:
  - **utoipa 6.** It adds a second schema derive across six crates, including `gateway-api-types`, against its stated dependency rule (`api-types/lib.rs:7-9`). Its required-field rule misdescribes always-emitted response fields (`utoipa-gen-6.0.1/src/component.rs:56-60`). Status lists would be restated by hand in attributes. Revisit if utoipa gains serialize/deserialize contracts.
  - **aide.** Stable 0.15.1 requires schemars 0.9. The schemars 1 line exists only as an alpha that has had no stable release in seven months, and adopting it means replacing `Router` with `ApiRouter` in every area. Revisit when a stable aide supports schemars 1.
  - **A hand-written document.** `crates/gateway/README.md:75` rules it out.
  - **Two artifacts, default and `--no-default-features`.** No test build can produce the latter (Decision D7).
  - **Describing upstream schemas for pass-through bodies.** The rule is that the Gateway documents only what it does.
- Assumptions, risks, and notes:

  | Risk | What catches it |
  |---|---|
  | A handler gains a new error path, and its declared error groups no longer list every status it can return | Conformance only when a case provokes the new status. **No test catches an unexercised new error path.** Mitigations: the error groups sit beside one another in `openapi/errors.rs`, each citing its producing function, and the conformance test prints every unexercised status |
  | An extension is classified as standard, or the reverse | **No test can**: it is a semantic judgment. Mitigation: the classification table above, cited, approved by the reviewer |
  | schemars renders a type differently from how serde actually writes it, or a schema-only type drifts from the `json!` it describes | Conformance, for the exercised responses. In-scope types have no custom `Serialize`. `CacheEntry.path` and the cache replies carry a `PathBuf` (`local/cache.rs:95`), whose serialization fails for a non-UTF-8 path, an error no schema can show |
  | A feature marker is wrong | `web-search`, `stt`, and `config-ui`: the reduced-build equation. `local`: the source check. A schema branch gated by `cfg` inside a type, such as `CatalogModelInfo::Speech`: the reduced build, because `stt` is off there |
  | A hand-declared request element drifts: the multipart form, the realtime `intent` parameter, `ErrorEnvelope`, or the SSE line shapes | The multipart `syn` check; a realtime probe asserting 400 for a wrong intent and not 400 for the documented one; conformance over every error builder and every SSE line |
  | Upstream-dependent behavior: the forwarded `Content-Type` (`app/relay.rs:200-210`, `app/speech.rs:192-197`) and upstream 4xx statuses | **No test can**: it depends on the provider. The document says so where it applies |
  | Router-level behavior outside handlers: 404 and 405 for unknown paths and methods | Not per operation in OpenAPI. The implicit `HEAD` probe covers the one router behavior that adds methods |
  | Doc extraction misses a doc attribute that a macro generates | Generation fails with `HandlerUndocumented` or `HandlerNotFound`; `include_str!` docs are banned (`crates/build-xtask/src/no_doctests.rs:1-6`) |
  | A schemars upgrade changes output | Drift fails and names the pointers. Rebless after review |
  | Release friction: every version bump changes `info.version` | Drift, by design |

  - **Note: a pre-existing failure.** `cargo deny check` fails at `9369a7d` before any change: `async-executor 1.14.0` (`Cargo.lock:212-213`, through `ksni` and `zbus`) is yanked, so `advisories FAILED, bans ok, licenses ok, sources ok`. This plan does not fix it.
  - **Note: no new lint exceptions.** None are anticipated. If one turns out to be needed, Phase 2 stops and asks.
  - **Note: the repository remote** is `sykosyber/runvil-gateway`, and `gh run list` shows no workflow runs there, so CI results were not available to check.

### Open questions

- **O1. Scope count.** Confirm 13 operations: the 11 you named plus the two STT routes.
- **O2. Unclassified routes.** `crates/gateway/README.md:23` names the routes with no standard but omits `/health`, `/v1/cache`, `/shutdown`, `/config`, `/auth`, and `/config/*`. The root `README.md:31` calls `/v1/cache` "A Gateway extension", and `README.md:32` lists `/config/`, `/admin/*`, and `/health` as "The Gateway's own settings page, admin surface, and liveness probe" without saying extension. Should `crates/gateway/README.md:23` name `/v1/cache` too, and is `/health` an extension?
- **O3. `ModelInfo.object`.** Is `object` (`api-types/metadata.rs:185-186`) standard or an extension? The doc names only `id` as OpenAI's (`:180`).
- **O4. Pass-through bodies.** Message, choice, embedding, rerank, and delta payloads stay `true` schemas by rule (D13). Is any upstream-side guarantee the Gateway enforces worth stating as a schema? For example, every choice has `index` and one of `message`, `delta`, or `text` (`protocol/wire.rs:167-187`), or the reply is refused with 502.
- **O5. Open-ended media types.** The chat SSE `Content-Type` and the speech `Content-Type` are forwarded from the upstream when valid (`app/relay.rs:200-205`; `app/speech.rs:192-197`). The document lists the fallback types and says the set is open. Is that the wanted form?
- **O6. `4XX` range.** Upstream 4xx statuses pass through as themselves (`protocol/error.rs:126-131`), so the document needs a `4XX` range entry. On the speech route that excludes 429, which is remapped (`app/speech.rs:107`).
- **O7. Oversized chat bodies.** A JSON body over 2 MiB is answered 400 `malformed_request`, not 413, because `WireJson` maps every rejection (`app/error/wire.rs:31-34`; `axum-core-0.5.6/src/ext_traits/request.rs:317-319`). The only raised limit is batch STT's (`stt/batch.rs:99`). Is the 2 MiB cap intended for chat requests with images?
- **O8. Speech SSE frames.** With `stream_format: "sse"`, the frames are the upstream's bytes, unread (`app/speech.rs:156-158`). The document can name only the media type.
- **O9. Batch `temperature`.** It is parsed and validated (finite, at least 0), then discarded (`stt/batch.rs:196-210`). Document it as "validated, not used"?
- **O10. Batch `words`.** It is always empty (`stt/batch.rs:81-83`, `:286`).
- **O11. Realtime message protocol.** It is outside OpenAPI's reach: is the stub in D8 enough until an AsyncAPI document exists?
- **O12. `trust_loopback`.** Is the prose and `x-runvil-loopback-trust` marker in D12 enough, given that OpenAPI cannot say "anonymous from loopback only"?
- **O13. `/auth` query rejection.** `/auth` uses plain axum `Query` (`app/admin/walled/handoff.rs:82`), so its rejection is plain text, unlike `WireQuery`'s envelope (`app/error/wire.rs:38-58`). It is excluded, but noted.
- **O14. Cache DELETE path rejection.** `DELETE /v1/cache/{sha256}` uses plain axum `Path<String>` (`app/cache.rs:171`), so an undecodable capture is axum's plain-text 400 rather than the envelope that `WirePath` gives (`app/error/wire.rs:60-80`). The document will state both. Is that inconsistency intended?
- **O15. Deferred decisions.** Should the open admin follow-up (D2), serving (D5), and the CI step (D7) be approved now or later?
- **O16. Typing the path-bearing cache replies.** `json!` interpolation calls `to_value(...).unwrap()` (`serde_json-1.0.151/src/macros.rs:279`), so a cache reply whose `PathBuf` is not valid UTF-8 panics today (`app/cache.rs:139-142`, `:335-338`). A typed reply would surface a serialization error instead, which is a behavior change. Step 3 therefore leaves those two literals alone and describes them with schema-only types pinned by conformance. Accept that, or approve the behavior change?

### Contradictions (the code is documented as it behaves; a human decides)

- **C1. The registry claims to be complete.** `app/registry.rs:8-11` and `app/lib.rs:32-36` call the registry the one enumerable route list. Seven mounted routes are outside it (Functional Specification, "Counts"), and `app/loopback-tests.rs:4` says so.
- **C2. A stub that does not exist.** `crates/gateway/app/Cargo.toml:139-142` says "a --no-default-features build stubs the route". No stub exists: without `stt`, `/v1/audio/transcriptions` is not mounted (`app/lib.rs:460-469` is the only mount, and it is `#[cfg(feature = "stt")]`).
- **C3. A dead models type.** `gateway_protocol::wire::ModelsResponse` (`protocol/wire.rs:243-251`) is documented as "The model list returned by `GET /v1/models` (the OpenAI shape)" but is used nowhere. The route returns `CatalogModelsResponse` (`app/models.rs:26-33`, `:71-104`).
- **C4. `/v1/models` versus OpenAI's model object.** `crates/gateway/README.md:7` says `/v1/models` follows OpenAI. The repository's own record of OpenAI's model object is "`id`, `created`, `owned_by`" (`vibe/2026-09/2026-09-14-2-provider-model-sheets.md:50`). `ModelInfo` (`api-types/metadata.rs:181-201`) and `SpeechCatalogModelInfo` (`app/models.rs:63-68`) emit neither `created` nor `owned_by`.
- **C5. Extensions that are not opt-in.** `crates/gateway/README.md:21` requires an extension to be opt-in "wherever the protocol offers a way to ask". `gateway_warning` is emitted whenever an emulated dialect hits a malformed fence, without any client request (`app/dialect.rs:104-111`). The `ModelInfo` extension fields are always emitted (`app/models.rs:81-89`). Whether that clause exempts them, because chat replies and the model list offer no way to ask, is for the reviewer.
- **C6. `[DONE]` after an error.** `relay_sse`'s doc says "a clean end is marked with the `data: [DONE]` sentinel" (`app/relay.rs:160-162`). The code chains `[DONE]` after every end, including after a mid-stream error event (`app/relay.rs:173-199`).
- **C7. Shutdown's path.** `app/auth/primitives.rs:93-94` names `POST /admin/shutdown`. The route is `POST /shutdown` (`app/admin/walled/shutdown.rs:23`).
- **C8. Routes that do not exist.** `loopback/peer.rs:14-18` lists "profile create and delete" among the walled endpoints. No such route exists in the registry or elsewhere (`rg 'RouteInfo::walled' crates`).
- **C9. An empty key.** `app/admin/walled/shutdown.rs:36-37` mentions "the deliberately credential-free empty-key configuration". Config validation refuses an empty server key: "server.key must not be empty" (`config/config/validate.rs:65-67`).
- **C10. "Always 200."** `/health`'s doc says "always 200 while serving" (`app/health.rs:20`). The host wall answers 403 on a loopback bind when the authority does not match, `/health` included (`loopback/host.rs:35-37`).
- **C11. Segments when not requested.** `VerboseJsonTranscription.segments` is documented as "Clip-level segments when segment granularity was requested" (`stt/batch.rs:79`). The granularity list starts as `[segment]` (`stt/batch.rs:44-46`, `:148`), and `timestamp_granularities[]` only appends (`:184-195`), so segments are always emitted for `verbose_json` (`:270`).
- **C12. Who the key gates.** `config/config.rs:320` says the bearer key is what "every `/v1/*` request must present". It gates every authenticated route, `/admin` included (`app/auth.rs:186-213`). And `config/config.rs:323-325` says loopback trust admits a keyless loopback peer "to every route", while `check_auth` also requires the Fetch Metadata rule (`app/auth.rs:205-210`).
- **C13. "Every route except `/health` is authenticated."** The root `README.md:36` says so. The five config UI assets take no credential, only the peer wall (`config-ui/routes.rs:15-23`), and `/config` takes no caller and redirects (`app/admin/walled/handoff.rs:50-54`; `app/admin/walled.rs:12-14`). `/auth` checks the key from its query string rather than through `check_auth` (`app/admin/walled/handoff.rs:84-88`).

### Missing or incomplete doc comments

- **M1.** The STT batch `handler` (`stt/batch.rs:103`) and the realtime `upgrade` (`stt/realtime/route.rs:130`) have no doc comment.
- **M2.** The `SpeechCatalogModelInfo` fields `id`, `object`, and `kind` (`app/models.rs:65-67`) have no doc comment.
- **M3.** `chat_completions` is documented only as "The chat route to a backend." (`app/relay.rs:80`), so its published description will be that one sentence plus the request and response type docs.
- **M4.** The error envelope has no Rust type. Three builders produce it independently (`app/error.rs:327-332`, `protocol/error.rs:150-155`, `stt/batch.rs:328-344`).
- **M5.** The `json!` replies (Decision D9) have no type and so no type docs. Step 3's typed replies and schema-only types take their docs from the handler docs that describe them (`app/cache.rs:103-110`, `:163-167`; `app/speech.rs:314-324`).
- **M6, outside the public scope.** `EndpointStatus` fields (`app/models.rs:111-114`), `ActiveCommandReply` and `PendingCommandReply` fields (`app/admin/open/status.rs:77-78`, `:85-86`), and `CancelPendingRequest.index` (`app/admin/open/queue.rs:50`) have no doc comment.

### Deferred and Out of Scope

- **Deferred: AsyncAPI for `/v1/realtime`.** It is the only way to describe the event protocol (`stt/realtime/wire/server.rs:143-210`).
- **Deferred: the open admin tier in the document** (Decision D2's follow-up).
- **Deferred: breaking versus additive classification** of spec changes, as `crates/gateway-api-types/tests/sheet_schema.rs:91-110` does for the sheet. `crates/gateway/README.md:63` forbids "Removing an extension without a version step", which such a check could enforce.
- **Out of scope:** serving the document, fixing any contradiction above, and the yanked `async-executor`.

</decision-record>
<project-survey>

## Project Survey

- Status: complete for Phase 1.
- Repository: `master` at `9369a7d`; remote `sykosyber/runvil-gateway`. Toolchain 1.99.1, pinned by `rust-toolchain.toml`.
- Baseline gates at `9369a7d`, Linux, run for this plan:
  - `npm ci --prefix crates/gateway/config-ui/ui`: ok.
  - `cargo nextest run --locked --workspace --all-features`: 1940 run, 1940 passed, 54 skipped; 86 s of test time, 2 min 54 s of wall time including the build.
  - `cargo test --locked -p build-xtask`: ok.
  - `CARGO_BUILD_WARNINGS=deny cargo clippy --locked --workspace --all-targets --all-features`: ok.
  - `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --all-features`: ok.
  - `cargo fmt --all --check`: ok.
  - `cargo test --locked -p gateway --no-default-features --no-run`: ok, in 64 s.
  - `cargo deny check`: fails on the yanked `async-executor` (Notes).
- Tools: cargo-nextest 0.9.151 and cargo-deny 0.20.2 were installed from their release binaries for this survey. Node 22.
- Crate facts checked: the crates.io API for utoipa, utoipa-gen, utoipa-axum, aide, schemars, oas3, openapiv3, openapiv3_1, jsonschema, and okapi (versions, dates, licenses, dependency requirements), plus the published source of each version cited.
  - `oas3` 0.22.0 parses OAS 3.1 but defaults to `serde_json/preserve_order`, which `deny.toml:63-67` bans, so it would need `default-features = false`. It was not chosen, because parsing into typed structures is not meta-schema validation.
- Scratch measurements, never committed:
  - A git worktree with `jsonschema = "=0.61.0"`, `default-features = false`, as a dev-dependency of `gateway` added 27 lock entries.
  - In that worktree, `cargo deny check bans licenses sources` passed, with one new duplicate-version warning (`allocator-api2`).
  - `cargo tree -p gateway -e normal -i jsonschema` printed nothing, confirming no normal edge.
- Conventions that bind Phase 2:
  - Kebab `-tests.rs` siblings wired with `#[path]`; three or more files make a directory (`app/lib.rs:45-55`).
  - Area modules own their mounts and bind `INFO.path` (`app/registry.rs:4-6`).
  - Every crate's build script runs the 500-line ceiling.
  - Comments explain only non-obvious constraints.
  - `crates/gateway/README.md:68-73`'s four questions apply to every wire change.
- Docs read: `AGENTS.md`, `README`, and the README and AGENTS files of every crate this plan touches (`crates/gateway/app`, `crates/gateway-api-types`, `crates/gateway/protocol`, `crates/gateway/stt`, `crates/gateway/stt/engine`, `crates/gateway/web-search`, `crates/gateway/local`, `crates/gateway/config`, `crates/gateway/config-ui`, `crates/gateway/routing`, `crates/shared-loopback`).

</project-survey>
<execution-plan>

## Execution Instructions

Phase 2 starts only after approval. Each step is one commit that leaves every exit criterion passing, and each commit carries a `Plan: vibe/2026-11/2026-11-09-1-gateway-openapi.md` trailer. Before editing a crate, read its README and AGENTS file.

<step-1>

### Step 1: Bring the seven external mounts into the registry [completed]

- Contract items: Decision D3; Functional Specification, "Counts".
- Artifacts:
  - `gateway-stt` exports `TRANSCRIPTIONS_PATH` and `REALTIME_PATH` and binds them at `stt/batch.rs:98` and `stt/realtime/route.rs:122`.
  - `gateway-config-ui` exports its asset paths and binds them at `config-ui/routes.rs:17-21`.
  - `app/registry.rs` adds the STT routes (open, `stt`) and the asset routes (walled, `config-ui`, prefixed with the `/config/` mount from `app/lib.rs:459`) under the same cfg gates `build_router` uses.
  - Correct `app/registry.rs:8-11`, `app/lib.rs:32-36`, and `app/loopback-tests.rs:4`.
- Behavior: the routes, their status codes, and their bodies are unchanged. Seven more debug log lines appear.
- Tests: `app/registry-tests.rs`, unedited, now sweeps 46 pairs. Plus the exit criteria.
- Commit: `Enumerate the STT and config UI mounts in the route registry`

</step-1>

<step-2>

### Step 2: Derive JsonSchema on the in-scope wire types [completed]

- Contract items: Technical Design, "File and public API changes"; Decisions D14 and D17.
- Artifacts:
  - `schemars.workspace = true` in the crates named in Decision D17.
  - `#[derive(JsonSchema)]` on the types listed in Technical Design, with the two speech constraints bound to the `validate()` constants.
  - The four `pub(crate)` widenings from Decision D16.
- Behavior: none. A derive adds a trait impl, and schemars attributes are inert to serde.
- Tests: the exit criteria, with `sheet_schema` passing unchanged.
- Commit: `Derive JSON Schemas for the public wire types`

</step-2>

<step-3>

### Step 3: Type the in-scope JSON literal replies [completed]

- Contract items: Decisions D9 and D19; missing doc M5.
- Artifacts: `HealthReply`, `VoicesReply` and `Voice`, `CacheDeletedReply`, and the cache `downloading` event type. Fields are declared in sorted key order, with doc comments taken from the handler docs that describe them. The cache hit reply and the terminal `ready` and `error` events stay `json!` (Decision D9, open question O16) and get schema-only types instead.
- Behavior: byte-identical bodies.
- Tests: one byte-equality test per reply against the replaced `json!` literal, plus the exit criteria.
- Commit: `Type the health, voices, and cache replies`

</step-3>

<step-4>

### Step 4: Generate, commit, and pin the document [completed]

- Contract items: Technical Design, "Document assembly", "Scope and exclusions", "The extension marking convention", and "Artifact, rendering, and bless"; Decisions D2, D4, D6, D10-D13, D18, D20, and D21.
- Artifacts:
  - `app/openapi.rs` and its children, each under 500 lines.
  - `crates/gateway/app/openapi.json`.
  - The field-level extension attributes from the approved classification.
  - The README section, the dev-dependencies `syn` and `proc-macro2`, and the `UNDOCUMENTED` list.
- Tests: coverage, mount binding, drift, loose schemas, multipart fields, implicit HEAD, and the exit criteria.
- Commit: `Generate the Gateway OpenAPI description and pin it`

</step-4>

<step-5>

### Step 5: Validate the document as OpenAPI 3.1 [completed]

- Contract items: Decision D15; Testing Plan, "Validity".
- Artifacts:
  - `jsonschema` in `[workspace.dependencies]` and as a dev-dependency of `gateway`.
  - `crates/gateway/app/tests/fixtures/oas-3.1/` with the four schema files and the Apache-2.0 notice.
  - The validity and structural tests.
- Tests: the validity tests, plus a negative control (a bad response code) asserted to fail, plus the exit criteria.
- Commit: `Validate the OpenAPI description against the OAS 3.1 schema`

</step-5>

<step-6>

### Step 6: Check feature markers [completed]

- Contract items: Decision D7.
- Artifacts: the reduced-build equation test, the `local` source check, and the new CI step.
- Tests: `cargo nextest run --locked -p gateway --no-default-features -E 'test(/openapi/)'`, plus the exit criteria.
- Commit: `Check the OpenAPI feature markers in reduced builds`

</step-6>

<step-7>

### Step 7: Conformance [completed]

- Contract items: Testing Plan, "Conformance".
- Artifacts: the conformance tests and the unexercised-status report.
- Tests: the exit criteria.
- Commit: `Validate route responses against the OpenAPI description`

</step-7>

<step-8>

### Step 8: Audit map and final report [completed]

- Contract items: the Definition of Done's audit map; contradictions C1-C13; missing docs M1-M6.
- Artifacts:
  - A test-only printer, `AUDIT_OPENAPI=1 cargo test -p gateway --lib openapi::audit -- --nocapture`, that prints every path, operation, and component with the `file:line` of its source, using `syn` span locations (`Cargo.toml:115-117`).
  - Its output pasted into this plan as the audit map. It is not committed as a drift-checked file, because line numbers move with every edit.
  - `crates/gateway/README.md:75` updated.
- Tests: every exit criterion, and the CI run on the branch.
- Commit: `Record the OpenAPI audit map`

</step-8>

</execution-plan>

**Audit map.**

Printed by `AUDIT_OPENAPI=1 cargo test -p gateway --lib openapi::audit -- --nocapture` on the default build. Line numbers move with the source, so this copy is a record of the implementation, not a second artifact.

```text
post /v1/audio/speech  crates/gateway/app/src/speech.rs:58  audio_speech
post /v1/audio/transcriptions  crates/gateway/stt/api/src/batch.rs:118  handler
get /v1/audio/voices  crates/gateway/app/src/speech.rs:347  audio_voices
get /v1/cache  crates/gateway/app/src/cache.rs:67  list_cache
post /v1/cache  crates/gateway/app/src/cache.rs:158  post_cache
delete /v1/cache/{sha256}  crates/gateway/app/src/cache.rs:215  delete_cache
post /v1/chat/completions  crates/gateway/app/src/relay.rs:81  chat_completions
post /v1/embeddings  crates/gateway/app/src/relay.rs:216  embeddings
get /v1/models  crates/gateway/app/src/models.rs:74  list_models
get /v1/realtime  crates/gateway/stt/api/src/realtime/route.rs:133  upgrade
post /v1/rerank  crates/gateway/app/src/relay.rs:245  rerank
post /v1/tools/web_search  crates/gateway/app/src/web_search.rs:33  web_search
schema CacheDeletedReply  crates/gateway/app/src/cache.rs:80  CacheDeletedReply
schema CacheDownloadEvent  crates/gateway/app/src/cache.rs:90  CacheDownloadEvent
schema CacheEntry  crates/gateway/local/src/cache.rs:92  CacheEntry
schema CacheErrorEvent  crates/gateway/app/src/cache.rs:117  CacheErrorEvent
schema CacheReadyReply  crates/gateway/app/src/cache.rs:106  CacheReadyReply
schema CacheRequest  crates/gateway/app/src/cache.rs:126  CacheRequest
schema CatalogModelInfo  crates/gateway/app/src/models.rs:39  CatalogModelInfo
schema CatalogModelsResponse  crates/gateway/app/src/models.rs:29  CatalogModelsResponse
schema ChatChunk  crates/gateway/protocol/src/wire.rs:197  ChatChunk
schema ChatChunkChoice  crates/gateway/protocol/src/wire.rs:232  ChatChunkChoice
schema ChatRequest  crates/gateway/protocol/src/wire.rs:33  ChatRequest
schema ChatResponse  crates/gateway/protocol/src/wire.rs:129  ChatResponse
schema EmbeddingInput  crates/gateway/protocol/src/wire/embedding.rs:11  EmbeddingInput
schema EmbeddingRequest  crates/gateway/protocol/src/wire/embedding.rs:20  EmbeddingRequest
schema EmbeddingResponse  crates/gateway/protocol/src/wire/embedding.rs:67  EmbeddingResponse
schema ErrorEnvelope  crates/gateway/app/src/error.rs:327  envelope
schema HealthReply  crates/gateway/app/src/health.rs:24  HealthReply
schema ModelInfo  crates/gateway-api-types/src/metadata.rs:191  ModelInfo
schema ModelKind  crates/gateway-api-types/src/metadata.rs:44  ModelKind
schema RerankRequest  crates/gateway/protocol/src/wire/rerank.rs:10  RerankRequest
schema RerankResponse  crates/gateway/protocol/src/wire/rerank.rs:62  RerankResponse
schema SpeechRequest  crates/gateway/protocol/src/wire/speech.rs:72  SpeechRequest
schema SpeechResponseFormat  crates/gateway/protocol/src/wire/speech.rs:40  SpeechResponseFormat
schema SpeechStreamFormat  crates/gateway/protocol/src/wire/speech.rs:62  SpeechStreamFormat
schema SpeechVoice  crates/gateway/protocol/src/wire/speech.rs:23  SpeechVoice
schema ThinkingMode  crates/gateway-api-types/src/metadata.rs:22  ThinkingMode
schema Voice  crates/gateway/app/src/speech.rs:321  Voice
schema VoicesReply  crates/gateway/app/src/speech.rs:331  VoicesReply
schema JsonTranscription  crates/gateway/stt/api/src/batch.rs:51  JsonTranscription
schema SpeechCatalogModelInfo  crates/gateway/app/src/models.rs:65  SpeechCatalogModelInfo
schema TranscriptionResponse  crates/gateway/stt/api/src/batch.rs:90  TranscriptionResponse
schema TranscriptionSegment  crates/gateway/stt/api/src/batch.rs:58  TranscriptionSegment
schema VerboseJsonTranscription  crates/gateway/stt/api/src/batch.rs:71  VerboseJsonTranscription
schema SearchResult  crates/gateway/web-search/src/service.rs:125  SearchResult
schema WebSearchRequest  crates/gateway/web-search/src/service.rs:86  WebSearchRequest
schema WebSearchResponse  crates/gateway/web-search/src/service.rs:116  WebSearchResponse
```
