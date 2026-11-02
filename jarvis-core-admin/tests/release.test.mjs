import assert from "node:assert/strict";
import test from "node:test";
import {
  cliCommand, isRunning, operationView, releaseHeadline, releaseOffer, releaseTag, runtimeBadge, track,
} from "../src/components/releases/release.ts";

const STATUS = { current: "v0.0.67", previous: "v0.0.66", latest: "v0.0.68", schema: "unchanged", updater: "disabled" };

test("only vMAJOR.MINOR.PATCH counts as a release tag", () => {
  assert.equal(releaseTag("v0.0.68"), "v0.0.68");
  for (const value of ["unavailable", "0.0.68", "v0.0", "v0.0.68 ", "", undefined]) assert.equal(releaseTag(value), "", String(value));
});

test("status offers a newer latest; a check's update key wins", () => {
  assert.deepEqual(releaseOffer(STATUS), { current: "v0.0.67", previous: "v0.0.66", install: "v0.0.68", migrate: "" });
  assert.equal(releaseOffer({ ...STATUS, latest: "v0.0.67" }).install, "");
  assert.equal(releaseOffer({ ...STATUS, latest: "v0.0.9" , current: "v0.0.10" }).install, "", "numeric, not string, compare");
  assert.equal(releaseOffer({ ...STATUS, update: "not available" }).install, "");
  assert.equal(releaseOffer({ ...STATUS, latest: "v0.0.67", update: "available" }).install, "v0.0.67");
  assert.equal(releaseOffer({ ...STATUS, update: "repair required" }).install, "");
  assert.equal(releaseOffer({ ...STATUS, latest: "unavailable" }).install, "");
  assert.equal(releaseOffer({ ...STATUS, current: "unavailable" }).install, "v0.0.68");
  assert.equal(releaseOffer({ ...STATUS, previous: "unavailable" }).previous, "");
});

test("a schema change offers Migrate, never Install", () => {
  const offer = releaseOffer({ ...STATUS, schema: "migration required", update: "available" });
  assert.equal(offer.migrate, "v0.0.68");
  assert.equal(offer.install, "");
  assert.equal(releaseOffer({ ...STATUS, schema: "migration required", latest: "unavailable" }).migrate, "");
});

test("headline and CLI equivalent name the version", () => {
  assert.equal(releaseHeadline(releaseOffer(STATUS)), "v0.0.68 is ready to install");
  assert.equal(releaseHeadline(releaseOffer({ ...STATUS, latest: "v0.0.67" })), "v0.0.67 is active");
  assert.equal(releaseHeadline(releaseOffer({ ...STATUS, schema: "migration required" })), "v0.0.68 needs a schema migration");
  assert.equal(releaseHeadline(releaseOffer({})), "Release status");
  assert.equal(cliCommand("version", "v0.0.68"), "sudo jarvis update --version v0.0.68");
  assert.equal(cliCommand("migrate", "v0.0.68"), "sudo jarvis update --migrate v0.0.68");
  assert.equal(cliCommand("rollback"), "sudo jarvis update --rollback");
  assert.equal(cliCommand("latest"), "sudo jarvis update --latest");
});

test("runtime badge: a failed check outranks an update", () => {
  assert.deepEqual(runtimeBadge(false, false, true), { tone: "idle", label: "Not installed" });
  assert.deepEqual(runtimeBadge(true, false, true), { tone: "error", label: "Needs attention" });
  assert.deepEqual(runtimeBadge(true, true, true), { tone: "warn", label: "Update available" });
  assert.deepEqual(runtimeBadge(true, true, false), { tone: "ok", label: "Installed" });
});

const LABELS = { doing: "Installing Core v0.0.68", done: "Install Core v0.0.68", note: "The updater owns it." };
const at = (h, m) => new Date(2026, 10, 2, h, m).getTime();
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };

test("a running operation maps to in progress, then to the last outcome with times", async () => {
  const store = { current: null };
  const work = deferred();
  const clock = [at(14, 2), at(14, 31)];
  const done = track(store, LABELS, () => work.promise, String, () => clock.shift());
  assert.ok(isRunning(store));
  assert.deepEqual(operationView(store.current), { phase: "running", badge: "In progress", headline: "Installing Core v0.0.68", detail: "Started 14:02. The updater owns it." });
  work.resolve({ success: true, summary: "Core operation completed", detail: "" });
  assert.equal((await done).success, true);
  assert.ok(!isRunning(store));
  assert.deepEqual(operationView(store.current), { phase: "done", badge: "Done", headline: "Last operation: Install Core v0.0.68", detail: "Finished 14:31 (started 14:02)." });
});

test("the outcome survives the caller: it is in the store, not in the view", async () => {
  const store = { current: null };
  const work = deferred();
  const done = track(store, LABELS, () => work.promise, String);
  // The view unmounts here (lock); the promise keeps running and writes to the store.
  work.resolve({ success: false, summary: "rollback completed after failed managed activation", detail: "" });
  await done;
  assert.equal(operationView(store.current).phase, "failed");
  assert.equal(store.current.result.summary, "rollback completed after failed managed activation");
});

test("a thrown error is a failed outcome; one operation at a time", async () => {
  const store = { current: null };
  const work = deferred();
  const first = track(store, LABELS, () => work.promise, (error) => error.message);
  let started = false;
  assert.equal(await track(store, LABELS, async () => { started = true; return { success: true }; }, String), null);
  assert.equal(started, false);
  work.reject(new Error("trusted administration session ended"));
  assert.equal(await first, null);
  assert.equal(store.current.error, "trusted administration session ended");
  assert.equal(operationView(store.current).badge, "Failed");
  // A finished operation does not block the next one.
  assert.equal((await track(store, LABELS, async () => ({ success: true, summary: "", detail: "" }), String)).success, true);
});
