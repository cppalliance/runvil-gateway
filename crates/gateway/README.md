# The gateway's API principle

The gateway serves an API that other programs already know how to call. Each route follows the industry standard it is modeled on, where one exists, so a program written against that standard works against the gateway, and a client written against the gateway works against any server that implements the standard.

## The rule

Standards first. The chat, embeddings, models, speech, and transcription routes follow OpenAI's API: `POST /v1/chat/completions`, `POST /v1/embeddings`, `GET /v1/models`, `POST /v1/audio/speech`, and `POST /v1/audio/transcriptions`. The live stream on `/v1/realtime` follows OpenAI's Realtime transcription protocol: the same session object, the same audio buffer events, and the same `conversation.item.input_audio_transcription.delta` and `conversation.item.input_audio_transcription.completed` events.

`/v1/rerank` is the one route where OpenAI publishes no shape, so it follows the rerank shape that Jina and Cohere publish and that vLLM also serves. A client written for that shape works against the gateway.

Interoperability is checked in both directions. A standard client pointed at the gateway needs no special case, and a gateway client pointed at another server that speaks the standard needs none either.

## Extensions, and nothing else

Where a standard exists and the gateway needs something the standard lacks, the wire shape is the standard plus the smallest addition that makes the feature possible. Everything around the addition stays the standard's: route, methods, request fields, event names, error envelope.

Every addition carries three properties:

- It is named as an extension, with a name inside the standard's own namespace, so a reader of the standard's vocabulary can tell what it extends.
- It is documented beside the standard shape it extends, so both are read in one place.
- It is opt-in wherever the protocol offers a way to ask. A client that does not ask never receives the extra field or event.

A few routes have no standard behind them. `GET /v1/audio/voices` follows the convention the OpenAI-compatible servers converged on, and `/v1/tools/web_search` and the `/admin` routes are ours alone. Those count as extensions from the start, and the same three properties apply to them.

## Asking what a server offers

A client has to know whether a server offers an extension before it relies on one, and the asking has to be harmless to a server that has never heard of the gateway. An absent answer means the standard alone, never that something is broken.

The one nonstandard request has two candidate shapes. A dedicated capability endpoint is the preferred one, because it costs one request at connect time and every answer sits in one place. A response header on a call the client already makes costs no request, but it ties the answer to a call the client might never make, so it waits as a later convenience. Either way the plain server answers with a not-found, a no-content, or an empty result, which the client already tolerates. The endpoint is part of the intended arrangement the way the spec is. This is the rule the gateway follows when it adds one, and no such endpoint exists yet.

The probe only reports. The feature turns on through the negotiated include token the protocol already has, and only when the probe said the server has it. A server that answers the probe and then refuses the token has a fault on its side, and the client calls that standard only, never an error to show the user.

So a missing or negative answer produces an ordinary session, one that appends deltas and completes an item, with no second attempt, no error banner, and no probe attached to each request. Nothing the client shows suggests that a step failed, because none did.

Guessing does not replace asking. A version string alone is not evidence, an extension request sent first with recovery after the error is not a probe, a 404 is not a hard failure, and no probe may change the shape of a standard response.

## The live hypothesis

The extension the gateway depends on most is the live hypothesis in realtime transcription.

OpenAI's transcription stream is append-only. The `delta` events add text to an item, and the `completed` event closes it. Nothing in that shape can retract a word already sent. The gateway revises earlier words as more audio arrives and as later passes listen again, so it needs a way to say that part of the transcript changed.

A replacement snapshot is the smallest addition that supports revision: the item's full current text, a revision number, the audio span the snapshot covers, and how that text divides into finalized, agreed, and tentative parts. The client rebuilds the take from the snapshot, then patches its document word by word against the text it showed before, so a revision costs a few small edits and leaves the rest of the document alone.

The snapshot is the extra. That division is information the standard path never needs, because a standard client only appends, and a fallback ignores it.

A session asks for the snapshot through `session.include`, the field OpenAI already uses to negotiate optional output, by naming `item.input_audio_transcription.hypothesis`. A session that never names it receives plain deltas and a completed transcript.

## Nothing clever reaches the wire

The gateway runs several speech models and decides which result wins. That machinery stays behind the API. Which model ran, how the gateway reconciled them, and how long any of it took never appear in a response or an event. A caller sees a transcription endpoint that happens to be more accurate, or earlier, than one model alone would be. It never sees a model pick.

## The fallback is a requirement

A client pointed at a server without the extension must still transcribe, with the plainer behavior the standard gives. It falls back to the standard and keeps working with less.

A feature that only works against the gateway is a bug in the design. The test is whether the client still works when the extension is absent.

## The ChatGPT plan

The gateway can sign a person in to their ChatGPT account and serve the models their plan includes. It is an extension in the sense above, and nothing changes for a client that does not ask. An endpoint asks by setting `auth = "chatgpt-plan"`. That setting requires `protocol = "openai-responses"` and allows neither `base_url` nor `api_key`, because a plan is sent to OpenAI only and with the signed-in person's credentials. Every other endpoint keeps the default, `auth = "api-key"`.

Two request headers carry the extension. Neither replaces the gateway's ordinary authentication, which a request passes first, and a header sent more than once counts as absent.

- `X-Runvil-Owner-Proof` shows that the caller can read `gateway-private.json`, the private file the gateway writes for its current boot. It gates the `/admin/chatgpt` routes, which also require the caller's own address to be loopback.
- `X-Runvil-Plan-Proof` shows that the caller is Runvil's own client. It gates the plan's models.

The sign-in exists only on a gateway that listens on loopback, meaning any address in `127.0.0.0/8` or `::1`. On any other gateway there is no sign-in, and the status route says so. The same terms ask that requests come from the user's own runtime.

The `/admin/chatgpt` routes are the gateway's own:

- `POST /admin/chatgpt/sign-in` starts a sign-in and returns one field, `authorize_url`, for the caller to open in a browser. The gateway listens for the browser's callback on `127.0.0.1`. A second call replaces the first.
- `GET /admin/chatgpt/status` reports where the sign-in stands, whether the plan grants use, and the plan's models. It never returns a token. A sign-in is reported as pending until its own work is finished, so a client that polls can stop at the first reply that is not pending. A warning that a token could not be revoked can still be set after a sign-out or a cancelled sign-in has returned, and a client sees it at its next read.
- `POST /admin/chatgpt/sign-out` signs out here and then revokes at OpenAI.
- `POST /admin/chatgpt/forget` signs out and drops the registration.
- `POST /admin/chatgpt/reset` deletes the saved record, readable or not.
- `POST /admin/chatgpt/models/enable` and `POST /admin/chatgpt/models/forget` add the plan's models to the configuration and remove them.

The standard routes keep their shapes. `GET /v1/models` lists a plan model only to a caller that presents the plan proof, and only while a readable sign-in with plan permission is active. To every other caller the list omits those rows. `POST /v1/chat/completions` for a plan model keeps the standard request, stream, and error shapes. On a gateway that listens on loopback, a request that names a plan model without the proof is refused with `403` and the code `chatgpt_plan_runvil_only`. On any other gateway the refusal comes first and its code is `chatgpt_plan_loopback_only`. The gateway does not substitute another model.

That refusal is the point of the plan proof. OpenAI's [Sign in with ChatGPT Terms](https://openai.com/policies/sign-in-with-chatgpt-terms/) restrict a plan to the application its user connected, in section 3.1: "Use the user's plan only for the application they connected. Do not provide general-purpose API access for other tools or unrelated requests." The plan proof is how this gateway keeps to that: it serves the plan to Runvil's own client and to nothing else.

`RUNVIL_CHATGPT_AGENT_NAME` changes the name the consent screen shows and nothing else. It takes 1 to 64 characters after trimming: ASCII letters and digits, the space, and `-_.()`. Any other value is ignored, and the name is `Runvil`.

## Saving the configuration

A whole-document save names the revision it was read from, so a page left open cannot overwrite a newer file. `GET /admin/config` returns the revision in `ETag` and in `x-config-revision` whenever the configuration came from a file. `PUT /admin/config` takes it back as `revision` in the body, as `x-config-revision`, or as `If-Match`.

- A save with no revision is refused with `428` and the code `config_revision_required`, and stages nothing.
- A save whose revision does not match the bytes it would replace is refused with `409` and the code `config_conflict`.
- A revision that is malformed, or two carriers that disagree, is refused with `422`.

A save is compared with the staged draft when there is one, and with the file otherwise. So once a draft is staged, the revision from `GET /admin/config` may no longer match. `GET /admin/config-pending` returns the revision a save will be compared with, in its `revision` field and in both headers.

## What this rules out

- A parallel dialect of our own where a standard shape would do.
- Our own event or field names where a standard one exists.
- Removing an extension without a version step.
- An extension a caller cannot ignore.

## Checking a change

Before a route or a wire type changes, answer four questions:

1. Does the standard shape still hold?
2. Is the addition the smallest one that makes the feature possible, and is it opt-in?
3. Does the client still work without the addition?
4. Is the addition named as an extension in the spec?

The spec belongs with the gateway. The intended arrangement is a spec generated from the gateway's source rather than written by hand, with CI comparing the two so a route or a field cannot drift from its description. That keeps the principle in a check instead of in someone's memory.
