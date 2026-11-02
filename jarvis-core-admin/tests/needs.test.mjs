import assert from "node:assert/strict";
import test from "node:test";
import { logServiceFor, needsYou, newerRelease, updaterLabel } from "../src/components/overview/needs.ts";

const LABELS = { anthropic: "Anthropic", openai: "OpenAI", deepseek: "DeepSeek" };
const ok = (data) => ({ data, error: "" });
const SERVICES = { Core: "active", SurrealDB: "active", "Config broker": "active", "Codex broker": "active", OpenSandbox: "active" };
const calm = () => ({
  overview: ok({ status: { release: "v0.1.0", services: { ...SERVICES }, updater_enabled: "disabled", agent_bundle: null }, update: { current: "v0.1.0", latest: "v0.1.0", schema: "unchanged" } }),
  usage: ok({ over_budget: false, spent_eur: 1, budget_eur: 50 }),
  models: ok([{ provider: "anthropic-api", enabled: true }, { provider: "deepseek-api", enabled: false }, { provider: "ollama", enabled: true }]),
  routes: ok({ routing: { version: 1, paid_api: "allowed", tiers: {} }, routing_unavailable_reason: null }),
  devices: ok({ devices: [], requests: [] }),
  credentials: ok([{ provider: "anthropic", configured: true }, { provider: "openai", configured: false }, { provider: "deepseek", configured: false }]),
});
const keys = (sources) => needsYou(sources, LABELS).items.map((item) => item.key);

test("nothing to do once every source read: empty list, not loading", () => {
  assert.deepEqual(needsYou(calm(), LABELS), { items: [], loading: false });
});

test("a source still loading keeps the list loading, never All good", () => {
  const sources = calm();
  sources.models = { data: null, error: "" };
  assert.deepEqual(needsYou(sources, LABELS), { items: [], loading: true });
});

test("usage still initializing in Core is not a failed check", () => {
  const sources = calm();
  sources.usage = { data: null, error: "usage statistics are unavailable while Core starts" };
  assert.deepEqual(needsYou(sources, LABELS), { items: [], loading: false });
});

test("a failed source becomes a refresh row, also when it kept stale data", () => {
  const sources = calm();
  sources.usage = { data: null, error: "broker down" };
  sources.devices = { data: { devices: [], requests: [] }, error: "timeout" };
  const { items, loading } = needsYou(sources, LABELS);
  assert.equal(loading, false);
  assert.deepEqual(items.map((item) => [item.key, item.target, item.detail]), [["unread", "refresh", "Could not read usage, devices."]]);
});

test("each pending pairing is its own Review row that opens Devices", () => {
  const sources = calm();
  sources.devices = ok({ devices: [], requests: [{ id: "r1", name: "Pixel", platform: "Android" }, { id: "r2", name: "iPad", platform: "iOS" }] });
  const items = needsYou(sources, LABELS).items;
  assert.deepEqual(items.map((item) => [item.key, item.action, item.target]), [["pairing:r1", "Review", "devices"], ["pairing:r2", "Review", "devices"]]);
  assert.equal(items[0].title, "Pixel wants to pair");
});

test("services not active get a row; failed is an error and logs map to the allowlist", () => {
  const sources = calm();
  sources.overview.data.status.services = { ...SERVICES, SurrealDB: "failed", OpenSandbox: "activating", Mystery: "inactive" };
  const items = needsYou(sources, LABELS).items;
  assert.deepEqual(items.map((item) => [item.key, item.tone, item.target, item.service]), [
    ["service:SurrealDB", "error", "logs", "surrealdb"], ["service:OpenSandbox", "warn", "logs", "opensandbox"], ["service:Mystery", "error", "refresh", undefined],
  ]);
  // "Open logs" preselects the unit, so the row no longer says to choose it.
  assert.equal(items[0].detail, "State: Failed.");
  assert.equal(logServiceFor("Config broker"), "config-broker");
  assert.equal(logServiceFor("Core"), "core");
  assert.equal(logServiceFor("Caddy"), null);
});

test("over budget, broken routing file", () => {
  const sources = calm();
  sources.usage = ok({ over_budget: true, spent_eur: 61.5, budget_eur: 50 });
  sources.routes = ok({ routing: null, routing_unavailable_reason: "routing.toml is invalid." });
  const items = needsYou(sources, LABELS).items;
  assert.deepEqual(items.map((item) => [item.key, item.tone, item.target]), [["budget", "error", "usage"], ["routing", "warn", "models"]]);
  assert.match(items[0].detail, /€61\.50 of €50\.00/);
});

test("update available from the status tags, or from a check; a migration replaces it", () => {
  const sources = calm();
  sources.overview.data.update = { current: "v0.1.9", latest: "v0.1.10", schema: "unchanged" };
  assert.deepEqual(keys(sources), ["update"]);
  sources.overview.data.update = { current: "v0.2.0", latest: "v0.1.10", schema: "unchanged" };
  assert.deepEqual(keys(sources), []);
  sources.overview.data.update = { current: "unavailable", latest: "v0.1.10", schema: "unchanged" };
  assert.deepEqual(keys(sources), []);
  sources.overview.data.update = { latest: "v0.2.0", update: "available", schema: "unchanged" };
  assert.deepEqual(needsYou(sources, LABELS).items.map((item) => [item.key, item.title, item.target]), [["update", "Update available: v0.2.0", "releases"]]);
  sources.overview.data.update = { latest: "v0.2.0", update: "available", schema: "migration required" };
  assert.deepEqual(keys(sources), ["migration"]);
  sources.overview.data.update = { update: "not available" };
  assert.deepEqual(keys(sources), []);
  sources.overview.data.update = null;
  assert.deepEqual(keys(sources), []);
});

test("a key is missing only when an enabled model or a routing tier uses it", () => {
  const sources = calm();
  sources.credentials = ok([{ provider: "anthropic", configured: false }, { provider: "openai", configured: false }, { provider: "deepseek", configured: false }, { provider: "jev", configured: false }]);
  assert.deepEqual(keys(sources), ["key:anthropic"]);
  sources.routes.data.routing.tiers = { hard: { chain: [{ provider: "openai-api", model: "gpt" }], metered_after_subscription: false } };
  const items = needsYou(sources, LABELS).items;
  assert.deepEqual(items.map((item) => [item.key, item.target]), [["key:anthropic", "credentials"], ["key:openai", "credentials"]]);
  assert.equal(items[1].title, "OpenAI key is not set");
});

test("release tags compare numerically; malformed tags are never newer", () => {
  assert.equal(newerRelease("v0.0.10", "v0.0.9"), true);
  assert.equal(newerRelease("v1.0.0", "v0.9.99"), true);
  assert.equal(newerRelease("v0.0.9", "v0.0.9"), false);
  assert.equal(newerRelease("v0.0.8", "v0.0.9"), false);
  assert.equal(newerRelease("v0.1", "v0.0.9"), false);
  assert.equal(newerRelease(undefined, "v0.0.9"), false);
});

test("the updater timer off reads as manual, also as the is-enabled fallback", () => {
  assert.equal(updaterLabel("disabled"), "Manual · timer off");
  assert.equal(updaterLabel("inactive"), "Manual · timer off");
  assert.equal(updaterLabel("enabled"), "Automatic · timer on");
  assert.equal(updaterLabel("masked"), null);
});
