import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  AUTOMATIC_UPDATE_DELAY_MS,
  shouldScheduleAutomaticUpdateCheck,
} from "../src/updatePolicy.js";

test("automatic update check runs once only after authentication", () => {
  assert.equal(shouldScheduleAutomaticUpdateCheck(false, false), false);
  assert.equal(shouldScheduleAutomaticUpdateCheck(true, false), true);
  assert.equal(shouldScheduleAutomaticUpdateCheck(true, true), false);
  assert.ok(AUTOMATIC_UPDATE_DELAY_MS >= 1000);
});

test("desktop API has no build-time or implicit localhost endpoint", async () => {
  const source = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");
  assert.doesNotMatch(source, /VITE_JARVIS_API_BASE/);
  assert.doesNotMatch(source, /localhost:8080/);
  assert.match(source, /homeNodeOrigin\(\)/);
});
