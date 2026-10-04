import assert from "node:assert/strict";
import test from "node:test";
import { NOT_MEASURED, budgetPercent, measured, ms, serviceSummary } from "../src/stats.ts";

test("unmeasured values are never shown as zero", () => {
  assert.equal(measured(null), NOT_MEASURED);
  assert.equal(measured(undefined), NOT_MEASURED);
  assert.equal(measured(0), "0");
  assert.equal(measured(1234), "1,234");
  assert.equal(measured(950, ms), "950 ms");
  assert.equal(measured(5100, ms), "5.1 s");
});

test("service summary counts active units and picks the orb tone", () => {
  assert.deepEqual(serviceSummary(null), { up: 0, total: 0, tone: "idle" });
  assert.deepEqual(serviceSummary({ core: "active", db: "active" }), { up: 2, total: 2, tone: "ok" });
  assert.deepEqual(serviceSummary({ core: "active", db: "inactive" }), { up: 1, total: 2, tone: "warn" });
  assert.deepEqual(serviceSummary({ core: "Failed", db: "active" }), { up: 1, total: 2, tone: "error" });
});

test("budget share is clamped and safe without a budget", () => {
  assert.equal(budgetPercent(5, 50), 10);
  assert.equal(budgetPercent(80, 50), 100);
  assert.equal(budgetPercent(5, 0), 0);
});
