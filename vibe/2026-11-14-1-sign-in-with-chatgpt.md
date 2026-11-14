---
name: Sign in with ChatGPT
overview: The Gateway signs a person in to their ChatGPT account through OpenAI's documented flow for open-source tools, keeps the credential to itself, and serves the plan's models to Runvil's own client and to nothing else. Config saves gain a revision check, because the Gateway now edits `gateway.toml` itself.
todos:
  - id: responses-upstream
    content: "gateway-protocol: the Responses request and stream translation, the bearer source, and a client that follows no redirect"
    status: completed
  - id: private-proofs
    content: "gateway-api-discovery: `gateway-private.json`, the owner proof and the plan proof of one boot"
    status: completed
  - id: chatgpt-plan-crate
    content: "gateway-chatgpt-plan: callback, ID-token checks, OAuth calls, the broker, the owner-only record, and a fake issuer for tests"
    status: completed
  - id: config-revision
    content: "gateway, gateway-config, config UI: a whole-document config save names the revision it was read from"
    status: completed
  - id: plan-routes-and-gate
    content: "gateway, gateway-config, gateway-routing, config UI: `auth = \"chatgpt-plan\"`, the `/admin/chatgpt` routes, the plan gate, and model setup"
    status: completed
  - id: verify
    content: "run every CI check on each commit"
    status: completed
isProject: false
---

# Sign in with ChatGPT

<product-contract>

## Product Requirements

- Problem and users:
  - A person with an eligible ChatGPT account wants to use the models their plan includes from Workshop, without an API key.
  - OpenAI documents a flow for open-source tools: <https://developers.openai.com/siwc/token-sharing-open-source>. The person authorizes one registered tool. The tool gets an access bearer for `POST /v1/responses` and `GET /v1/models`, a refresh token for the issuer's OAuth endpoints, and an ID token that names the account.
  - The Gateway already holds every other provider credential. Workshop and the Harness hold none.
- Goals:
  - The whole flow stays in the Gateway process: registration, sign-in, ID-token checks, the saved refresh token, refresh, and revocation.
  - An endpoint with `auth = "chatgpt-plan"` serves its models only to a caller that passes ordinary authentication and presents `X-Runvil-Plan-Proof`, on a Gateway that listens on loopback.
  - The `/admin/chatgpt` routes answer only to a loopback caller that passes ordinary authentication and presents `X-Runvil-Owner-Proof`.
  - A whole-document config save names the revision it was read from, so it cannot overwrite a file the Gateway or another page changed since.
- Non-goals:
  - Serving a plan to any client other than Runvil's own. OpenAI's Sign in with ChatGPT Terms restrict a plan to the application its user connected, in section 3.1: "Use the user's plan only for the application they connected. Do not provide general-purpose API access for other tools or unrelated requests." The plan proof is how this Gateway keeps to that.
  - A plan endpoint with its own `base_url` or `api_key`.
  - A sign-in on a Gateway that listens on any address other than loopback.
  - A saved credential on Windows. The store refuses to save there until an owner-only file is enforced.
  - The owner proof on the admin routes that existed before this change.
- Success criteria:
  - A person signs in on macOS or Linux and runs a plan model from Workshop.
  - A request for a plan model without the plan proof is refused with `403 chatgpt_plan_runvil_only`. `GET /v1/models` omits plan rows for that caller and still answers `200`. Models that are not plan models are unaffected.
  - A config save with no revision is refused with `428 config_revision_required`, and one with a stale revision with `409 config_conflict`.
  - Every CI check passes.
- Constraints:
  - The flow follows OpenAI's pages. Each module names the page and section it follows.
    - Registration and sign-in: <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>
    - Token reference: <https://developers.openai.com/siwc/token-sharing-open-source/token-reference>
    - Errors and recovery: <https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery>
    - Models and inference: <https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference>
  - The Sign in with ChatGPT Terms, <https://openai.com/policies/sign-in-with-chatgpt-terms/>, effective 4 November 2026. Section 1 requires that a saved token is local and under the user's control. Section 3.1 requires that requests come from the user's own runtime and that the plan serves only the application the user connected.
  - `crates/gateway/README.md`: a standard route keeps its standard shape, and every addition is named as an extension and is opt-in.
- Open questions: None

## Functional Specification

- Inputs and outputs:
  - `[[endpoint]]` accepts `protocol = "openai-responses"` and `auth = "chatgpt-plan"`. The second requires the first and allows neither `base_url` nor `api_key`.
  - `<run_dir>/gateway-private.json` holds `pid`, `epoch`, `plan_proof`, and `owner_proof`. Each proof is 64 lowercase hex characters, minted at start. `pid` and `epoch` match `gateway.json`. On Unix the file has mode `0600`.
  - `POST /admin/chatgpt/sign-in` starts a one-shot callback listener on `127.0.0.1` and returns `{ authorize_url }`. A second call replaces the first.
  - `GET /admin/chatgpt/status` returns `status`, `unreadable`, `email`, `plan_granted`, `revocation`, `cleanup_unconfirmed`, `sign_in_pending`, `available`, `unavailable_code`, `unavailable_reason`, and `plan_models`. It never returns a token.
  - `POST /admin/chatgpt/sign-out` signs out locally, then revokes at OpenAI. `POST /admin/chatgpt/forget` also drops the registration.
  - `POST /admin/chatgpt/reset` deletes the saved record, readable or not, and returns `204`.
  - `POST /admin/chatgpt/models/enable` and `POST /admin/chatgpt/models/forget` add the plan's endpoint and models to `gateway.toml` and to live routing, and remove them.
  - `GET /admin/config` returns the revision in `ETag` and `x-config-revision` when the configuration came from a file. `GET /admin/config-pending` returns it in both headers and in a `revision` field. `PUT /admin/config` takes it as `revision` in the body, as `x-config-revision`, or as `If-Match`.
  - `RUNVIL_CHATGPT_AGENT_NAME` changes the name the consent screen shows. It takes 1 to 64 characters after trimming: ASCII letters and digits, the space, and `-_.()`. Any other value is ignored and the name is `Runvil`.
- States and validation:
  - A sign-in is signed out, active, or in need of a new sign-in, or its record is unreadable.
  - An ID token is verified as RS256 against the issuer's JWKS, and then `iss`, `aud`, `exp` with 60 seconds of skew, `nonce`, `at_hash`, and `sub` are checked in that order.
  - A plan request goes to a compiled destination, always streams, and never asks the provider to store the response.
  - Whether the Gateway listens on loopback is decided once, from the bound address: `127.0.0.0/8` or `::1`.
  - A Gateway removes its proofs file when it shuts down. One that shuts down to restart keeps its discovery file for its supervisor and still removes its proofs file.
  - An access bearer is cached and counts as usable until 120 seconds before it expires.
  - The status reports a sign-in as pending from the start of an attempt until that attempt's own work is finished: the exchange, the activation, and the revocation of a token it will not keep. A sign-out, a reset, or a newer attempt ends the pending state at once. An attempt whose callback never arrives stops being pending when it expires. A client that polls can therefore stop at the first status that is not pending.
  - A warning that a token could not be revoked can be set after a sign-out or a cancelled sign-in has returned. A client sees it at its next status read. Nothing notifies it.
- Errors and recovery:
  - A request that names a plan model without the plan proof: `403 chatgpt_plan_runvil_only`. On a Gateway that does not listen on loopback the refusal is `chatgpt_plan_loopback_only` and comes first.
  - An `/admin/chatgpt` request without the owner proof, after it passed the loopback wall and ordinary authentication: `403 owner_proof_required`.
  - A plan model with no usable sign-in: `403` with `chatgpt_sign_in_required`, `chatgpt_sign_in_expired`, or `chatgpt_plan_not_granted`, or `503 chatgpt_unavailable`.
  - A config save with no revision: `428 config_revision_required`. A stale one: `409 config_conflict`. A malformed one, or two carriers that disagree: `422`. Model setup while a draft is staged: `409 config_has_pending_changes`.
  - `reset` clears the current cleanup warning. A revocation that was still pending can fail later and raise the warning again.
- Security and privacy behavior:
  - Neither proof replaces ordinary authentication. A proof header that is sent more than once counts as absent.
  - The routing table's entry for a plan model holds an upstream that refuses. The upstream that spends the plan is handed out only by `ChatGpt::authorize_plan`, after it checks the listener mode, the exact proof, the broker, and the sign-in's state.
  - The upstream adapters build each outbound request themselves, so an incoming proof header is not forwarded.
  - The callback page is a fixed sentence. Nothing from the request is written to it or to a log.
  - `Debug` redacts both proofs. On the private file it prints `[REDACTED]` in their place.

</product-contract>
<implementation-contract>

## Technical Design

- Architecture:
  - `gateway-protocol`: `wire/responses.rs` rewrites a chat completion as a Responses request. `upstream/responses/` translates the Responses stream back to chat chunks. `upstream/bearer.rs` is the `BearerSource` an upstream asks for its credential. `http_util` gains `hardened_client` and `no_redirect_streaming_client`, which never fall back to a client that follows redirects.
  - `gateway-config`: the vocabulary gains `Protocol::OpenaiResponses` and the `chatgpt-plan` auth, and validation enforces what a plan endpoint may hold.
  - `gateway-api-discovery`: `GatewayPrivateFile`, written beside `gateway.json` through the same owner-only writer.
  - `gateway-chatgpt-plan`: `callback`, `id_token`, `oauth`, `plan` (the broker), `store`, and a `fake_issuer` that exists in tests and behind the `test-fixtures` feature.
  - `gateway`: one `ChatGpt` handle lives across config reloads. `auth/owner.rs` is the `OwnerCaller` extractor. `admin/walled/chatgpt.rs` and `admin/walled/chatgpt/` are the routes. `chatgpt-models.rs` fetches the plan's model list.
  - Config UI: a plan endpoint shows no `base_url` or `api_key`, a cloud merge skips it, and a save sends the revision.
- Data model:
  - `<profile>/chatgpt-plan/chatgpt-plan.json`: a version, the agent host id, the client id, the account's subject and email, the session with its refresh token and scopes, and a flag for cleanup that was not confirmed. It is written through a fresh `0600` temp file and renamed, and its directory must be private to the user.
- Interfaces and APIs:
  - Headers: `X-Runvil-Plan-Proof`, `X-Runvil-Owner-Proof`, `x-config-revision`, `ETag`, `If-Match`.
  - Routes: the seven under `/admin/chatgpt/` listed above.
- Failure handling:
  - A hardened client that cannot be built is kept as `None`, and each request then fails closed.
  - `resolve_routed_model` applies the plan gate for chat, embeddings, rerank, and speech, before the kind check.
  - Model enable captures a fence on the sign-in and commits only if the fence still holds. Sign-out and model forget supersede an enable that is in flight. A revocation that is still pending is tracked by operation id.
- Observability:
  - A refresh logs one info line with no token.
- Performance and scale:
  - Routing is read under the existing async lock, as before. Nothing here is lock-free.

</implementation-contract>
<verification-contract>

## Testing Plan

- Unit:
  - `gateway-protocol`: the request mapping, the stream translation including tool-call arguments, and the refusal when the no-redirect client is absent.
  - `gateway-chatgpt-plan`: the callback's fixed page, each ID-token rejection in order, PKCE, refresh rotation and revocation against the fake issuer, the fences under races, and the store's refusal rules.
  - `gateway-api-discovery`: the private file's mode, its handling of a corrupt file, and the redaction.
  - `gateway`: the gate across ordinary authentication, both proofs, and both listener modes, on chat, the model list, embeddings, rerank, and speech; the `/admin/chatgpt` routes; the enable and forget races; the `428` and `409` on saves.
  - Config UI: the settings page, the cloud merge, and the config store.
- Controls: 71 hand-written mutations, each of one named rule and each killed by a named test. The rules are in the gate, the owner routes, the ID-token checks, the fences, the store, the config revision, and the Responses translation. They cover the mutations they name, not every branch. The tests of a client that cannot be built inject the kept `None`, not a failing builder.
- Exit criteria:
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets --all-features`
  - `cargo nextest run --locked --workspace --all-features`
  - `cargo check --locked -p gateway --no-default-features`
  - `cargo doc --locked --workspace --no-deps --all-features` with `RUSTDOCFLAGS="-D warnings"`
  - `cargo xtask tidy`
  - `cargo deny check`
  - `npm run typecheck` and `npm test` in `crates/gateway/config-ui/ui`

</verification-contract>
<decision-record>

## Decision Record

- Decisions:
  - The credential stays in the Gateway. It already holds every provider credential, and a client that never has the token cannot leak it.
  - A plan model needs a proof on top of ordinary authentication. Loopback alone would serve the plan to every local program, which the terms' section 3.1 forbids. The proof is minted per boot and never written to the public discovery file.
  - Two proofs, not one. The plan proof lets a client use the models. The owner proof lets a caller manage the credential. A client that may use the models need not be able to manage the credential.
  - Config saves carry a revision. Model setup makes the Gateway a second writer of `gateway.toml`, beside the settings page. The check covers writers that go through the Gateway. It is not a lock against another program that edits the file.
  - The Responses protocol is its own endpoint protocol with a bearer source, not a special case inside chat completions.
- Reuse considered:
  - The private file uses the discovery file's owner-only writer. The routes use the existing loopback wall and authentication.

### Deferred and Out of Scope

- Deferred: the owner proof on the older admin routes (`PUT /admin/config`, `/admin/env`, `/admin/config-apply`).
- Deferred: a typed permit around plan routing. Today the refusing table entry and the runtime gate enforce it.
- Deferred: a saved credential on Windows.
- Deferred: removing `gateway-private.json` at shutdown by `epoch` as well as `pid`, and a route that forces a refresh.
- Out of scope: Windows was not run at all. No route forces a token refresh, so the failure cases of refresh are tested against the fake issuer only.

</decision-record>
<execution-plan>

## Execution Instructions

<step-1>

### Step 1: The Responses upstream [completed]

- Component: `responses-upstream`
- Tests:
  - `cargo nextest run --locked -p gateway-protocol --all-features`

Add the request mapping, the stream translation, `BearerSource`, and the two clients that follow no redirect.

Commit: one commit. Subject: `Add a Responses upstream and a bearer source to gateway-protocol`.

</step-1>

<step-2>

### Step 2: The private proofs file [completed]

- Component: `private-proofs`
- Tests:
  - `cargo nextest run --locked -p gateway-api-discovery --all-features`

Add `GatewayPrivateFile` beside the discovery file.

Commit: one commit. Subject: `Add the private file for a boot's owner and plan proofs`.

</step-2>

<step-3>

### Step 3: The gateway-chatgpt-plan crate [completed]

- Component: `chatgpt-plan-crate`
- Tests:
  - `cargo nextest run --locked -p gateway-chatgpt-plan --all-features`

Add the crate. Nothing depends on it yet.

Commit: one commit. Subject: `Add the gateway-chatgpt-plan crate`.

</step-3>

<step-4>

### Step 4: A revision on config saves [completed]

- Component: `config-revision`
- Tests:
  - `cargo nextest run --locked -p gateway -p gateway-config --all-features`
  - `npm test` in `crates/gateway/config-ui/ui`

`GET /admin/config` returns the revision, and `PUT /admin/config` refuses a save that names none or a stale one. The settings page sends it. This comes before the plan, because the plan's model setup is a second writer of `gateway.toml`.

Commit: one commit. Subject: `Require a revision on whole-document config saves`.

</step-4>

<step-5>

### Step 5: Routes, the gate, and model setup [completed]

- Component: `plan-routes-and-gate`
- Tests:
  - `cargo nextest run --locked --workspace --all-features`
  - `npm test` in `crates/gateway/config-ui/ui`

Add the config vocabulary, the routes, the gate, model setup, the settings page changes, and the README section. Model setup changes the config under the apply lock, and the revision check of step 4 stops a settings page that was open from overwriting that result with a later save.

Commit: one commit. Subject: `Serve ChatGPT plan models to Runvil's own client`.

</step-5>

</execution-plan>
