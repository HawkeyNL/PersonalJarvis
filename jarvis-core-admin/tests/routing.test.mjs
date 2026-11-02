import assert from "node:assert/strict";
import test from "node:test";
import {
  applyConfirm, applyInOrder, baseline, changes, isPaidModel, kind, modelConfirm, restage, routeConfirm, stageConfirm,
} from "../src/components/models/routing.ts";

const sub = { provider: "claude-cli", model: "claude-haiku-4-5" };
const paid = { provider: "zai-api", model: "glm-5.3-flash" };
const report = (routing = {}) => ({
  routing: { version: 1, paid_api: "allowed", research_web_search: "off", tiers: { cheap: { chain: [paid, sub], metered_after_subscription: false } }, ...routing },
  routing_unavailable_reason: null,
});
const row = (provider, model = "m", extra = {}) => ({ provider, model, enabled: false, source: "", route: null, price_status: "known", input_per_million_usd: 1, cache_read_per_million_usd: null, output_per_million_usd: 2, pricing_source: "", pricing_updated_at: "", ...extra });

test("an untouched baseline has no changes, and editing it never touches the report", () => {
  const saved = report();
  const staged = baseline(saved);
  assert.deepEqual(changes(saved, staged), []);
  staged.tiers.cheap.chain.reverse();
  assert.deepEqual(saved.routing.tiers.cheap.chain, [paid, sub]);
});

test("a missing routing file stages the built-in defaults", () => {
  const staged = baseline({ routing: null, routing_unavailable_reason: null });
  assert.deepEqual(staged, { paid_api: "allowed", research_web_search: "off", tiers: { cheap: null, default: null, hard: null } });
});

test("changes come in page order with the requests the old buttons sent", () => {
  const saved = report();
  const staged = baseline(saved);
  // Edited bottom-up on purpose: the order follows the page, not the edits.
  staged.tiers.hard = { chain: [sub], metered_after_subscription: false };
  staged.tiers.cheap = null;
  staged.research_web_search = "on";
  staged.paid_api = "off";
  staged.tiers.default = { chain: [sub, paid], metered_after_subscription: true };
  assert.deepEqual(changes(saved, staged).map((change) => change.request), [
    { action: "paid_api", state: "off" },
    { action: "research_web_search", state: "on" },
    { action: "reset", tier: "cheap" },
    { action: "set", tier: "default", chain: [sub, paid], metered_after_subscription: true },
    { action: "set", tier: "hard", chain: [sub], metered_after_subscription: false },
  ]);
});

test("toggling back, or reordering back, is no change", () => {
  const saved = report();
  const staged = baseline(saved);
  staged.paid_api = "off"; staged.paid_api = "allowed";
  staged.tiers.cheap.chain.reverse(); staged.tiers.cheap.chain.reverse();
  // An order started and emptied on a built-in tier is still the built-in order.
  staged.tiers.hard = { chain: [], metered_after_subscription: false };
  assert.deepEqual(changes(saved, staged), []);
});

test("reorder and metered flag are changes; invalid orders block Apply", () => {
  const saved = report();
  const staged = baseline(saved);
  staged.tiers.cheap.chain.reverse();
  let [change] = changes(saved, staged);
  assert.equal(change.request.action, "set");
  assert.match(change.problem, /paid API follows a subscription/);
  staged.tiers.cheap.metered_after_subscription = true;
  [change] = changes(saved, staged);
  assert.equal(change.problem, undefined);
  staged.tiers.cheap.chain = [];
  [change] = changes(saved, staged);
  assert.match(change.problem, /add a model or use the built-in order/);
});

test("Apply sends one change at a time, in order, and stops at the first failure", async () => {
  const saved = report();
  const staged = baseline(saved);
  staged.paid_api = "off";
  staged.tiers.cheap = null;
  staged.tiers.hard = { chain: [sub], metered_after_subscription: false };
  const list = changes(saved, staged);
  const sent = [];
  let inFlight = 0;
  const send = async (request) => {
    assert.equal(inFlight++, 0, "never two calls at once");
    sent.push(request.action);
    await new Promise((resolve) => setImmediate(resolve));
    inFlight--;
    if (request.action === "reset") throw new Error("core not ready");
    return { success: true, summary: request.action, detail: "" };
  };
  const done = [];
  const outcome = await applyInOrder(list, send, () => true, (result, index) => done.push([result.summary, index]));
  assert.deepEqual(sent, ["paid_api", "reset"]);
  assert.deepEqual(done, [["paid_api", 0]]);
  assert.equal(outcome.applied, 1);
  assert.equal(outcome.error.message, "core not ready");

  // Core now has paid APIs off; the failed and unsent changes stay staged.
  const after = report({ paid_api: "off" });
  const next = restage(after, staged, list.slice(outcome.applied).map((change) => change.key));
  assert.deepEqual(changes(after, next).map((change) => change.request.action), ["reset", "set"]);
});

test("restage keeps a failed tier order as edited, and an emptied saved tier stays blocked", () => {
  const saved = report();
  const staged = baseline(saved);
  staged.tiers.cheap = { chain: [sub, paid], metered_after_subscription: true };
  staged.tiers.default = { chain: [], metered_after_subscription: false };
  const kept = restage(saved, staged, ["cheap", "default"]);
  assert.deepEqual(kept.tiers.cheap, { chain: [sub, paid], metered_after_subscription: true });
  assert.notEqual(kept.tiers.cheap, staged.tiers.cheap, "a copy, not the old draft");
  assert.deepEqual(changes(saved, kept).map((change) => change.request.action), ["set"]);
  const emptied = baseline(saved);
  emptied.tiers.cheap.chain = [];
  const [blocked] = changes(saved, restage(saved, emptied, ["cheap"]));
  assert.match(blocked.problem, /add a model or use the built-in order/);
});

test("Apply stops when the page is locked or left mid-way", async () => {
  const saved = report();
  const staged = baseline(saved);
  staged.paid_api = "off"; staged.research_web_search = "on";
  let alive = true;
  const sent = [];
  const outcome = await applyInOrder(changes(saved, staged), async (request) => { sent.push(request.action); alive = false; }, () => alive);
  assert.deepEqual(sent, ["paid_api"]);
  assert.deepEqual(outcome, { applied: 1 });
});

test("discard is a fresh baseline of what Core has saved", () => {
  const saved = report();
  const staged = baseline(saved);
  staged.paid_api = "off";
  staged.tiers.cheap = null;
  assert.equal(changes(saved, staged).length, 2);
  assert.deepEqual(changes(saved, baseline(saved)), []);
});

test("after a full apply restage keeps nothing", () => {
  const saved = report();
  const staged = baseline(saved);
  staged.paid_api = "off";
  const after = report({ paid_api: "off" });
  assert.deepEqual(changes(after, restage(after, staged, [])), []);
});

test("Apply confirm lists every change, says Core restarts, and is danger when it can bill or removes an order", () => {
  const saved = report();
  const restrictive = baseline(saved);
  restrictive.paid_api = "off";
  let spec = applyConfirm(changes(saved, restrictive));
  assert.equal(spec.tone, "primary");
  assert.match(spec.detail, /Applying restarts Jarvis Core/);
  assert.match(spec.detail, /1\. Paid APIs: turn off/);

  const reset = baseline(saved);
  reset.tiers.cheap = null;
  reset.research_web_search = "on";
  spec = applyConfirm(changes(saved, reset));
  assert.equal(spec.tone, "danger");
  assert.match(spec.detail, /once per change/);
  assert.match(spec.detail, /1\. Research web search: turn on\n2\. Cheap: use the built-in order/);
  assert.match(spec.detail, /No memory, history or device data is sent/);

  const allow = baseline(report({ paid_api: "off" }));
  allow.paid_api = "allowed";
  spec = applyConfirm(changes(report({ paid_api: "off" }), allow));
  assert.equal(spec.tone, "danger");
  assert.match(spec.detail, /Providers bill each call/);

  const metered = baseline(saved);
  metered.tiers.cheap.metered_after_subscription = true;
  assert.equal(applyConfirm(changes(saved, metered)).tone, "danger");
});

test("staging confirms: allow paid is danger, research on is primary, reset is danger and names the tier", () => {
  assert.equal(stageConfirm("allow_paid").tone, "danger");
  assert.equal(stageConfirm("research_on").tone, "primary");
  const reset = stageConfirm("reset", "Hard");
  assert.equal(reset.tone, "danger");
  assert.match(reset.title, /Hard/);
  for (const spec of [stageConfirm("allow_paid"), stageConfirm("research_on"), reset]) assert.match(spec.detail, /Nothing changes until you apply/);
});

test("model confirms: enabling a paid model is danger, others primary, disable stays danger", () => {
  assert.equal(kind("claude-cli"), "subscription");
  assert.equal(kind("ollama"), "local");
  assert.equal(kind("ollama-cloud"), "paid API");
  assert.equal(kind("some-new-provider"), "paid API", "unknown providers count as paid");
  assert.equal(isPaidModel(row("huggingface")), true);
  assert.equal(isPaidModel(row("codex-cli")), false);
  assert.equal(modelConfirm("enable", row("openai-api", "gpt-x")).tone, "danger");
  assert.match(modelConfirm("enable", row("openai-api", "gpt-x")).detail, /bills each call/);
  assert.equal(modelConfirm("enable", row("ollama", "qwen3")).tone, "primary");
  assert.equal(modelConfirm("enable", row("claude-cli", "opus")).tone, "primary");
  assert.deepEqual(modelConfirm("disable", row("openai-api", "gpt-x")), {
    title: "Disable this model?", detail: "openai-api/gpt-x will no longer be eligible for routing.", label: "Disable gpt-x", tone: "danger",
  });
});

test("HF route confirm names the model and route", () => {
  const spec = routeConfirm(row("huggingface", "org/model"), "fastest");
  assert.equal(spec.tone, "primary");
  assert.match(spec.detail, /org\/model.*"fastest"/);
  assert.match(spec.detail, /does not enable this model/);
});
