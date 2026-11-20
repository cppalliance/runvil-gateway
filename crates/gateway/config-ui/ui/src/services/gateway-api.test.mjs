// Pins the refusal threading every route shares: a refusal from a
// route other than config-apply throws a GatewayHttpError that includes the
// envelope's `error.code` alongside its message, so callers can branch
// on the code (the way the desk words the apply_cancelled toast).
import assert from "node:assert/strict";
import test from "node:test";

import { jsonResponse, loadApp } from "../test-support.mjs";

/** A minimal Storage stand-in holding one verified key. */
function storageShim() {
  const map = new Map([["gateway-api-key", "k"]]);
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, String(value)),
    removeItem: (key) => map.delete(key),
  };
}

test("a putConfig refusal includes the envelope's error code", async () => {
  const app = await loadApp();
  const fetchFn = async () =>
    jsonResponse(
      {
        error: {
          message: "unknown field `typo`",
          type: "invalid_request_error",
          code: "config_invalid",
        },
      },
      422,
    );
  const api = new app.GatewayApi({ fetchFn, storage: storageShim() });

  await assert.rejects(api.putConfig({}, "a".repeat(64)), (error) => {
    assert.ok(error instanceof app.GatewayHttpError, "the refusal throws GatewayHttpError");
    assert.equal(error.status, 422);
    assert.equal(error.message, "unknown field `typo`", "the envelope message is kept");
    assert.equal(error.code, "config_invalid", "the error includes the envelope code");
    return true;
  });
});

test("putConfig refuses a missing or empty revision before touching the transport", async () => {
  const app = await loadApp();
  let calls = 0;
  const fetchFn = async () => {
    calls += 1;
    return jsonResponse({ shadow: "gateway.toml.next" }, 200);
  };
  const api = new app.GatewayApi({ fetchFn, storage: storageShim() });

  for (const bad of ["", "   ", null, undefined]) {
    await assert.rejects(api.putConfig({}, bad), /revision token/i);
  }
  assert.equal(calls, 0, "no unconditional PUT /admin/config may be sent");
});
