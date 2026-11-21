---
name: Check the body of a plain chat reply
overview: A chat request that does not ask to stream is answered with the backend's whole reply as one JSON body. Tests check the status of that answer and what the Gateway forwarded, and they check the text of a streamed reply. No test reads the body of a plain reply, so the Gateway can return an empty object, or the wrong model name, and every test still passes. This plan adds one test. It changes no production code.
todos:
  - id: plan-copy
    content: "Copy this plan to vibe/2026-11/2026-11-20-1-plain-chat-reply-body.md on the branch qa-plan-tests and commit it alone"
    status: completed
  - id: test
    content: "Add the test to crates/gateway/app/tests/it/chat.rs"
    status: completed
  - id: breaks
    content: "Apply each of the two breaks, confirm the test now fails, and revert it"
    status: completed
  - id: verify
    content: "Run the verification commands"
    status: completed
  - id: pr
    content: "Push the branch and open one PR against cppalliance/runvil-gateway when the user asks"
    status: pending
isProject: false
---

# Check the body of a plain chat reply

<product-contract>

## Product Requirements

The Gateway serves an OpenAI-shaped API, so that a client written against OpenAI works here unchanged. Most such clients send a chat request without `stream` and read one JSON body back. `chat_completions` in `crates/gateway/app/src/relay.rs` answers that request with the reply its backend gave. No test reads that body, so it can be broken while all 1,962 existing tests pass. The code is correct today, so this is missing protection, not a bug.

- Problem and users:
  - With the plain reply's body replaced by an empty JSON object, every test passes.
  - With the plain reply naming the backend's own model name in place of the public name the caller asked for, every test passes.
  - The users are the people who point an OpenAI client at the Gateway, and the engineers who change the relay and rely on its tests.
- Goals:
  - Each of the two breaks listed in the Functional Specification makes at least one test fail.
- Non-goals:
  - Any change to production code.
  - A test with a real model or a stock OpenAI SDK.
  - The streamed reply and the emulated tool dialect. Both have tests that read the reply.
- Success criteria:
  - With each break applied in turn, `cargo nextest run -p gateway` fails, and it passes again once the break is reverted.
  - The workspace has exactly one more test than before: 1,963 run, up from 1,962, on a default build.
  - The verification commands in the Testing Plan pass.
- Constraints:
  - Every existing test in `crates/gateway/app/tests/it/chat.rs` keeps every assertion it makes today.
- Open questions: None

## Functional Specification

One test sends a plain chat request to a Gateway whose backend is the suite's fake OpenAI backend, and reads the answer's body. Nothing a user of the Gateway sees changes.

- Actors and workflows:
  - A developer or CI runs the test suite. No other actor is involved.
- Inputs and outputs:
  - The request asks the public model `test-model` for a reply to the message `ping`, with a valid key and no `stream` field.
  - The answer has status 200. Its body is the backend's reply whole: the id, the object `chat.completion`, one choice with the assistant message `pong` and the finish reason `stop`, and the model `test-model`, the public name the caller asked for.
- States and validation: None
- Errors and recovery: None
- Security and privacy behavior: None
- Acceptance criteria:
  - Break 1: in `chat_completions`, answer a plain request that is not on the emulated dialect with `Json(serde_json::json!({}))`. The test fails.
  - Break 2: in `chat_completions`, set `response.model` to `model.upstream_name` before the plain reply is returned. The test fails.
  - With either break applied, the new test is the only test in the workspace that fails.

</product-contract>
<implementation-contract>

## Technical Design

The change is one test function in an existing file. No module, interface, or stored format changes.

- Architecture:
  - The test starts the Gateway on a loopback port over the fake backend, as the other tests of the file do.
- Modules and interfaces: None
- File and public API changes:
  - `crates/gateway/app/tests/it/chat.rs` gains one test. It uses the support the file already imports: `gateway_for`, `fake_backend`, `chat_body`, `send_within`, `json_within`, and `canned_reply`.
  - `vibe/2026-11/2026-11-20-1-plain-chat-reply-body.md` is this plan.
  - No route, wire type, or OpenAPI document changes.
- Data, persistence, failure, security, and privacy constraints: None

</implementation-contract>
<verification-contract>

## Testing Plan

The change is itself a test, so its own check is that each break fails it.

- Unit: None
- Integration and end-to-end:
  - `cargo nextest run --locked -p gateway --test it -E 'test(a_plain_chat_request_is_answered_with_the_backends_reply)'` runs the new test.
- Regression, security, and performance:
  - Apply each of the two breaks in turn, run the whole workspace, see the new test fail and every other test pass, and revert the break.
- Exit criteria:
  - `cargo nextest run --locked --workspace --all-features`
  - `cargo clippy --locked --workspace --all-targets --all-features`
  - `cargo fmt --all --check`
  - `cargo doc --locked --workspace --no-deps --all-features`, with `RUSTDOCFLAGS="-D warnings"`
  - `cargo test -p build-xtask`

</verification-contract>
<decision-record>

## Decision Record

- Decisions:
  - One new test function, not a check added to `forwards_method_path_model_and_messages_to_backend`. That test's backend records the request and answers with a bare reply, and its subject is what the Gateway forwards.
  - The body is compared whole with the suite's `canned_reply`, so a field the Gateway drops or changes fails the test.
  - Both gaps were shown by breaking the code. Break 1 was applied before the test was written, and all 1,962 tests passed. Break 2 is shown by the run that proves the test: with it applied, every test but the new one passes.
- Rejected alternatives:
  - A stock OpenAI SDK as the client. It would add a dependency to say the same thing: the body has the OpenAI shape. Revisit if a real SDK is found to reject a reply this test accepts.
- Assumptions, risks, and notes:
  - `stream_true_relays_typed_chunks_and_done` already reads a streamed reply's text, and the tests of the emulated dialect read its rewritten reply.
  - This serves the test "Point an OpenAI client at the Gateway and send one chat request. A reply comes back." of the QA plan of attack.
  - Result of the breaks: on a default build the workspace runs 1,963 tests. Under each break the new test fails and the other 1,962 pass.
  - Result of the exit criteria: every command passed. With all features the workspace runs 1,964 tests.
  - `vibelight` was run on `chat.rs` alone, before and after. It reports one more lead after: the new test repeats the request the file's other tests build, with the same key and URL. Sharing that code would mean rewriting the existing tests, which this plan does not do.

### Deferred and Out of Scope

- Deferred: a chat request with a key while loopback trust is off. `trust_loopback_false_refuses_the_keyless_loopback_client` checks two other routes. Revisit as its own step.

</decision-record>
<project-survey>

## Project Survey

None

</project-survey>
<execution-plan>

## Execution Instructions

- Work items:
  - Commit this plan alone on the branch `qa-plan-tests`, made from `origin/master`.
  - Add the test to `crates/gateway/app/tests/it/chat.rs`.
  - Apply and revert each break, recording which test fails.
  - Run the exit criteria.
  - Commit the test. Push the branch and open the PR only when the user asks.
- Dependencies and verification:
  - A default build needs one `npm ci` in `crates/gateway/config-ui/ui` first.
  - The breaks are never committed. `git status` shows no change to `relay.rs` before each commit.

</execution-plan>
