import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  AUTOMATIC_UPDATE_DELAY_MS,
  shouldScheduleAutomaticUpdateCheck,
} from "../src/updatePolicy.js";

test("automatic update check runs once only after authentication", () => {
  assert.equal(shouldScheduleAutomaticUpdateCheck(false, true, false), false);
  assert.equal(shouldScheduleAutomaticUpdateCheck(true, false, false), false);
  assert.equal(shouldScheduleAutomaticUpdateCheck(true, true, false), true);
  assert.equal(shouldScheduleAutomaticUpdateCheck(true, true, true), false);
  assert.ok(AUTOMATIC_UPDATE_DELAY_MS >= 1000);
});

test("desktop API has no build-time or implicit localhost endpoint", async () => {
  const source = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");
  assert.doesNotMatch(source, /VITE_JARVIS_API_BASE/);
  assert.doesNotMatch(source, /localhost:8080/);
  assert.match(source, /homeNodeOrigin\(\)/);
});

test("production capabilities exclude development loopback HTTP", async () => {
  const production = await readFile(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8");
  const development = await readFile(new URL("../src-tauri/capabilities/development.json", import.meta.url), "utf8");
  assert.doesNotMatch(production, /localhost|127\.0\.0\.1|http:\/\//);
  assert.match(development, /http:\/\/localhost:8080/);
});

test("session bearer is never exposed through the desktop JavaScript API", async () => {
  const api = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");
  const auth = await readFile(new URL("../src/auth.ts", import.meta.url), "utf8");
  const native = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.doesNotMatch(api, /Authorization|Bearer/);
  assert.doesNotMatch(auth, /auth_session|auth_save|token:\s*string/);
  assert.doesNotMatch(native, /generate_handler![\s\S]*auth_session/);
  assert.match(native, /auth_complete_login/);
  assert.match(native, /async fn auth_request/);
});
