import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";
import { accountConfirm, keyConfirm, keyUseLabel } from "../src/components/accounts/accounts.ts";
import { keyInUse, keyUse } from "../src/components/overview/needs.ts";

const MODELS = [
  { provider: "openai-api", enabled: true }, { provider: "openai-api", enabled: true }, { provider: "openai-api", enabled: false },
  { provider: "deepseek-api", enabled: false }, { provider: "ollama", enabled: true },
];
const ROUTES = { routing: { tiers: { fast: { chain: [{ provider: "deepseek-api", model: "d" }, { provider: "ollama", model: "o" }] } } }, routing_unavailable_reason: null };

test("key usage maps credential ids to model providers: enabled models and routing", () => {
  assert.deepEqual(keyUse("openai", MODELS, ROUTES), { models: 2, routing: false });
  assert.deepEqual(keyUse("deepseek", MODELS, ROUTES), { models: 0, routing: true });
  assert.deepEqual(keyUse("anthropic", MODELS, ROUTES), { models: 0, routing: false });
  assert.deepEqual(keyUse("xai", MODELS, { routing: null, routing_unavailable_reason: "bad file" }), { models: 0, routing: false });
  assert.equal(keyUse("jev", MODELS, ROUTES), null);
  assert.equal(keyInUse(keyUse("openai", MODELS, ROUTES)), true);
  assert.equal(keyInUse(keyUse("deepseek", MODELS, ROUTES)), true);
  assert.equal(keyInUse(keyUse("anthropic", MODELS, ROUTES)), false);
  assert.equal(keyInUse(null), false);
});

test("key usage lines, including unknown usage", () => {
  assert.equal(keyUseLabel({ models: 1, routing: true }), "Used by 1 enabled model and routing");
  assert.equal(keyUseLabel({ models: 3, routing: false }), "Used by 3 enabled models");
  assert.equal(keyUseLabel({ models: 0, routing: true }), "Used by routing");
  assert.equal(keyUseLabel({ models: 0, routing: false }), "Not used by enabled models or routing");
  assert.equal(keyUseLabel(null), "Task classifier key, not used by chat models");
  assert.match(keyUseLabel(undefined), /^Usage unknown/);
});

test("reconnect and replace confirm with the provider name in primary tone; disconnect is danger", () => {
  const reconnect = accountConfirm("claude", "Claude Code", "reconnect");
  assert.equal(reconnect.tone, "primary");
  assert.equal(reconnect.title, "Reconnect Claude Code?");
  assert.equal(reconnect.confirmLabel, "Reconnect Claude Code");
  assert.match(reconnect.detail, /trusted terminal window/);
  const disconnect = accountConfirm("codex", "Codex", "disconnect");
  assert.equal(disconnect.tone, "danger");
  assert.equal(disconnect.confirmLabel, "Disconnect Codex");
  assert.match(disconnect.detail, /Codex chat worker, broker and coding service/);
  assert.doesNotMatch(accountConfirm("claude", "Claude Code", "disconnect").detail, /Codex/);
  const replace = keyConfirm("OpenAI", true);
  assert.equal(replace.tone, "primary");
  assert.equal(replace.title, "Replace the OpenAI key?");
  assert.equal(replace.confirmLabel, "Replace OpenAI key");
  assert.match(replace.detail, /trusted terminal window/);
});

test("connect, test and setting a missing key need no confirmation", () => {
  assert.equal(accountConfirm("claude", "Claude Code", "connect"), null);
  assert.equal(accountConfirm("codex", "Codex", "test"), null);
  assert.equal(keyConfirm("OpenAI", false), null);
});

// Secrets and logins go only through the trusted terminal. No .vue file may
// render a password field or an input that looks like it takes a secret.
const vueFiles = (dir) => readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
  const path = `${dir}/${entry.name}`;
  return entry.isDirectory() ? vueFiles(path) : entry.name.endsWith(".vue") ? [path] : [];
});

test("no .vue file contains a password or secret-like input", () => {
  const files = vueFiles(new URL("../src", import.meta.url).pathname);
  assert.ok(files.length > 10, "found the Vue sources");
  const SECRET = /password|passwd|secret|\btoken\b|access[-_ ]?key|api[-_ ]?key|credential|bearer|cookie/i;
  for (const file of files) {
    const source = readFileSync(file, "utf8");
    assert.doesNotMatch(source, /type\s*=\s*["']password["']|:type\s*=|-webkit-text-security|contenteditable/i, file);
    for (const [tag] of source.matchAll(/<(input|textarea)\b[^>]*>/gis)) {
      assert.doesNotMatch(tag, SECRET, `${file}: ${tag}`);
      assert.doesNotMatch(tag, /v-bind\s*=/, `${file}: spread attributes hide the input type: ${tag}`);
    }
  }
});
