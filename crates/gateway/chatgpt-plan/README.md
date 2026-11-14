# Gateway ChatGPT plan

This crate holds and manages a user's Sign in with ChatGPT credentials inside the Gateway process. It signs the user in through OpenAI's self-serve flow for open-source local tools, verifies every ID token, keeps the refresh token in an owner-only record in the profile directory, refreshes and revokes tokens, and supplies short-lived access bearers to authorized internal Gateway callers (`gateway_protocol::upstream::BearerSource` for Responses upstream requests, and `ChatGptPlan::access_token` for the `gateway` app's ChatGPT model catalog adapter in `app/src/chatgpt-models.rs::fetch_bytes`). HTTP wire replies, status snapshots, and logs do not expose tokens.

What exists here:

- `callback`: a one-shot listener on `127.0.0.1:0` for `GET /auth/callback`. The page is a fixed sentence. Nothing from the request is written to the page or to a log.
- `id_token`: RS256 verification against the issuer's JWKS, then `iss`, `aud`, `exp` (60 s skew), `nonce`, `at_hash`, and `sub`, in that order.
- `oauth`: dynamic registration, PKCE authorization URL generation, code exchange, refresh, and token revocation over bounded no-redirect HTTP requests.
- `plan`: serialized in-memory broker managing the session lifecycle (`ChatGptPlan`: sign-in, completion, cached/refreshed access bearers, session fencing, sign-out, forget, and reset). Its status reports a sign-in as pending from the start of an attempt until that attempt's own work is finished. A sign-out, a reset, or a newer attempt ends that at once, and an attempt whose callback never arrives stops being pending when it expires.
- `store`: the `chatgpt-plan.json` record, written through a fresh `0600` temp file and renamed, in a directory that must be a private directory of this user. A corrupt record is reported, never guessed at. Windows refuses to store until an owner-only DACL is enforced.

The protocol is OpenAI's documented flow for open-source tools that use a person's ChatGPT plan:

- Overview: <https://developers.openai.com/siwc/token-sharing-open-source>
- Registration and sign-in: <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>
- Token reference: <https://developers.openai.com/siwc/token-sharing-open-source/token-reference>
- Errors and recovery: <https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery>
- UI/UX guidelines, for whatever shows the sign-in to a person: <https://developers.openai.com/siwc/ui-ux-guidelines>
- The terms: <https://openai.com/policies/sign-in-with-chatgpt-terms/>. Section 1 requires that a saved token is local and under the user's control. Section 3.1 requires that the plan serves only the application the user connected.
