// Pins the store's discovered-model staging against the persisted
// selection: a Discover download joins the profile the pending envelope
// names (not the running one), and with no selection it joins the
// catalog alone while the toast says so.
import assert from "node:assert/strict";
import test from "node:test";

import {
  bootApp,
  gatewayStub,
  hfModelFixture,
  hfSearchFixture,
  modelsFixture,
  navigate,
  settle,
  systemFixture,
} from "../test-support.mjs";

const REPO = "unsloth/Qwen3-Test-8B-GGUF";

/** Boots to Discover, opens the fixture repo, and stages its first quant. */
async function stageFirstQuant(selected) {
  const config = modelsFixture();
  const stub = gatewayStub({
    key: "k",
    config,
    pending: structuredClone(config),
    selected,
    hfSearch: hfSearchFixture(),
    hfModels: { [REPO]: hfModelFixture() },
    system: systemFixture(),
  });
  const booted = await bootApp({ key: "k", stub });
  navigate(booted.dom, "#/discover");
  await settle();
  booted.root.querySelector(".result-row").click();
  await settle();
  booted.root.querySelector(".quant-download").click();
  await settle();
  const staged = stub.state.pending.local_model.find((entry) =>
    String(entry.source).startsWith("https://huggingface.co/"),
  );
  assert.ok(staged, "the quant joined the pending local catalog");
  const members = (name) =>
    stub.state.pending.profile.find((profile) => profile.name === name).models;
  return { ...booted, stub, staged, members };
}

test("a discovered model joins the selected profile, not the running one", async () => {
  const { root, staged, members } = await stageFirstQuant("travel");
  assert.ok(members("travel").includes(staged.name), "the persisted selection gains the model");
  assert.ok(
    !members("default").includes(staged.name),
    "the running profile is not the target when the selection differs",
  );
  assert.ok(
    [...root.querySelectorAll(".toast-success")].some((toast) =>
      toast.textContent.includes(staged.name),
    ),
    "the success toast names the staged model",
  );
});

test("with no selected profile the model joins the catalog alone and the toast says so", async () => {
  const { root, staged, members } = await stageFirstQuant(null);
  assert.ok(!members("default").includes(staged.name), "no profile gains the model");
  assert.ok(!members("travel").includes(staged.name));
  assert.ok(
    [...root.querySelectorAll(".toast")].some((toast) =>
      /no profile is selected/i.test(toast.textContent),
    ),
    "the toast says no profile is selected",
  );
});

test("the Review diff is empty when pending equals running while a profile runs", async () => {
  const config = modelsFixture();
  const stub = gatewayStub({
    key: "k",
    profile: "alpha",
    config,
    pending: structuredClone(config),
    dirty: { dirty: true, pending_files: ["gateway.toml"], changed_sections: [] },
  });
  const { dom, root } = await bootApp({ key: "k", stub });
  const doc = dom.window.document;

  root.querySelector(".banner-review").click();
  await settle();
  const overlay = doc.querySelector(".review-overlay");
  assert.ok(overlay, "Review opens");
  assert.equal(
    overlay.querySelector(".diff-table"),
    null,
    "no row renders: the running profile is not a document difference",
  );
  assert.match(overlay.textContent, /No visible value changes/);
});

test("the persisted selection never appears in a config PUT", async () => {
  const { stub } = await stageFirstQuant("travel");
  const puts = stub.calls.filter(
    (call) => call.url.endsWith("/admin/config") && call.init.method === "PUT",
  );
  assert.ok(puts.length > 0, "staging wrote the pending config");
  assert.ok(
    puts.every((call) => !("active_profile" in JSON.parse(call.init.body))),
    "the envelope's active_profile is stripped before the payload is built",
  );
  assert.ok(
    puts.every((call) => {
      const rev = JSON.parse(call.init.body).revision;
      return typeof rev === "string" && rev.length === 64;
    }),
    "every config PUT carries the loaded 64-hex revision precondition",
  );
});

test("a stale Settings save after out-of-band ChatGPT model enablement is refused with 409 and tells the user to reload", async () => {
  const config = modelsFixture();
  const stub = gatewayStub({
    key: "k",
    config,
    pending: structuredClone(config),
    models: ["qwen-common"],
  });
  const { dom, root } = await bootApp({ key: "k", stub });

  navigate(dom, "#/settings/gateway");
  await settle();

  // Out-of-band update on disk (ChatGPT sign-in + Add models) while Settings is open.
  stub.state.config.endpoint.push({
    id: "chatgpt",
    protocol: "openai-responses",
    auth: "chatgpt-plan",
  });
  stub.state.config.model.push({
    name: "gpt-5",
    description: "GPT-5",
    context: 400000,
    upstream: "gpt-5",
    endpoints: ["chatgpt"],
  });
  stub.state.pending = structuredClone(stub.state.config);

  const bind = root.querySelector(".field-row[data-key='bind'] input");
  bind.value = "127.0.0.1:9090";
  bind.dispatchEvent(new dom.window.Event("change"));
  await settle();
  root.querySelector(".card-save").click();
  await settle();

  const errorToasts = [...root.querySelectorAll(".toast-error")].map((el) => el.textContent);
  assert.ok(
    errorToasts.some((text) => /reload/i.test(text)),
    `409 refusal tells the user to reload, got: ${JSON.stringify(errorToasts)}`,
  );
  assert.ok(
    stub.state.pending.model.some((model) => model.name === "gpt-5"),
    "out-of-band ChatGPT plan model on disk was not overwritten",
  );
  assert.equal(
    root.querySelector(".field-row[data-key='bind'] input").value,
    "127.0.0.1:9090",
    "unsaved form input stays intact after the refusal",
  );
});

test("consecutive saves advance the tracked revision seamlessly", async () => {
  const config = modelsFixture();
  const stub = gatewayStub({
    key: "k",
    config,
    pending: structuredClone(config),
    models: ["qwen-common"],
  });
  const { dom, root } = await bootApp({ key: "k", stub });

  navigate(dom, "#/settings/gateway");
  await settle();

  const bind = () => root.querySelector(".field-row[data-key='bind'] input");
  bind().value = "127.0.0.1:9091";
  bind().dispatchEvent(new dom.window.Event("change"));
  await settle();
  root.querySelector(".card-save").click();
  await settle();

  bind().value = "127.0.0.1:9092";
  bind().dispatchEvent(new dom.window.Event("change"));
  await settle();
  root.querySelector(".card-save").click();
  await settle();

  const puts = stub.calls
    .filter((call) => call.url.endsWith("/admin/config") && call.init.method === "PUT")
    .map((call) => JSON.parse(call.init.body));
  assert.equal(puts.length, 2, "both consecutive saves succeeded");
  assert.notEqual(puts[0].revision, puts[1].revision, "second save used the updated shadow revision");
  assert.equal(stub.state.pending.server.bind, "127.0.0.1:9092");
});

test("save refuses locally when the loaded snapshot lacks a revision token", async () => {
  const config = modelsFixture();
  const stub = gatewayStub({
    key: "k",
    config,
    pending: structuredClone(config),
    models: ["qwen-common"],
  });
  stub.state.omitPendingRevision = true;
  const { dom, root } = await bootApp({ key: "k", stub });

  navigate(dom, "#/settings/gateway");
  await settle();

  const bind = root.querySelector(".field-row[data-key='bind'] input");
  bind.value = "127.0.0.1:9099";
  bind.dispatchEvent(new dom.window.Event("change"));
  await settle();
  root.querySelector(".card-save").click();
  await settle();

  const puts = stub.calls.filter(
    (call) => call.url.endsWith("/admin/config") && call.init.method === "PUT",
  );
  assert.equal(puts.length, 0, "no unconditional PUT /admin/config is sent when revision is missing");
  const errorToasts = [...root.querySelectorAll(".toast-error")].map((el) => el.textContent);
  assert.ok(
    errorToasts.some((text) => /reload/i.test(text)),
    `missing revision tells the user to reload, got: ${JSON.stringify(errorToasts)}`,
  );
});
