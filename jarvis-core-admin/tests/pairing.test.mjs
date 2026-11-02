import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  ACTIVITY_EVENTS, PAIRING_LIFETIME_S, REFRESH_INTERVAL_MS, REFRESH_TIMEOUT_MS,
  countdown, createActivityRefresh, groupFingerprint, isExpired, requestedAt, secondsLeft, withTimeout,
} from "../src/components/devices/pairing.ts";

const flush = () => new Promise((resolve) => setImmediate(resolve));
const FINGERPRINT = "0123456789abcdef".repeat(4);

test("fingerprint shows all 64 characters in 16 groups of four, like the apps", () => {
  const grouped = groupFingerprint(FINGERPRINT);
  assert.equal(grouped, "0123 4567 89ab cdef ".repeat(4).trimEnd());
  assert.equal(grouped.length, 79);
  assert.equal(grouped.replaceAll(" ", ""), FINGERPRINT);
  assert.equal(groupFingerprint("abcdef"), "abcd ef");
  assert.equal(groupFingerprint(""), "");
});

test("countdown runs down to Expired at the expiry second", () => {
  const expiresAt = 1_000_000; // Unix seconds
  const at = (seconds) => (expiresAt - seconds) * 1000;
  assert.equal(countdown(expiresAt, at(300)), "5:00");
  assert.equal(countdown(expiresAt, at(65)), "1:05");
  assert.equal(countdown(expiresAt, at(1)), "0:01");
  assert.equal(countdown(expiresAt, at(0.4)), "0:01");
  assert.equal(isExpired(expiresAt, at(0.4)), false);
  assert.equal(countdown(expiresAt, at(0)), "Expired");
  assert.equal(isExpired(expiresAt, at(0)), true);
  assert.equal(isExpired(expiresAt, at(-60)), true);
  assert.equal(secondsLeft(expiresAt, at(-60)), 0);
});

test("requested time is the Core lifetime before expiry", () => {
  assert.equal(PAIRING_LIFETIME_S, 300);
  assert.equal(requestedAt(1_000_300).getTime(), 1_000_000_000);
});

test("withTimeout rejects a hanging request and clears its timer otherwise", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const hanging = withTimeout(new Promise(() => {}), 50);
  t.mock.timers.tick(50);
  await assert.rejects(hanging, /did not answer in time/);
  assert.equal(await withTimeout(Promise.resolve("ok"), 50), "ok");
});

// A refresher listening on a fake shell, with mocked clock and timers.
function setup(t, ready) {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval", "Date"], now: 1_000_000 });
  const calls = [];
  let resolve;
  const refresher = createActivityRefresh(() => {
    calls.push(Date.now());
    return new Promise((r) => { resolve = r; });
  }, ready);
  const shell = new EventTarget();
  refresher.start(shell);
  t.after(() => refresher.stop());
  return { calls, refresher, shell, finish: async () => { resolve(); await flush(); } };
}
const activity = (shell, name = "pointermove") => shell.dispatchEvent(new Event(name));

test("no activity means no refresh: idle never polls", (t) => {
  const { calls } = setup(t);
  t.mock.timers.tick(10 * 60 * 1000);
  assert.equal(calls.length, 0);
});

test("refreshes only on shell activity, at most once per interval, at the moment of that activity", async (t) => {
  const { calls, shell, finish } = setup(t);
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 0, "the mount load counts as the latest refresh");

  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  assert.equal(calls.length, 0, "time passing alone never refreshes");
  const activeAt = Date.now();
  activity(shell, "keydown");
  t.mock.timers.tick(0);
  assert.deepEqual(calls, [activeAt], "the broker call starts with the activity, so it cannot outlast the idle lock");
  await finish();

  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 1, "throttled to the interval");
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell, "focusin");
  t.mock.timers.tick(0);
  assert.equal(calls.length, 2);
});

test("listens to exactly the shell's activity events", () => {
  const app = readFileSync(new URL("../src/App.vue", import.meta.url), "utf8");
  const shell = app.match(/<div class="app-shell"([^>]*)>/)[1];
  const events = [...shell.matchAll(/@([a-z]+)="recordActivity"/g)].map((match) => match[1]);
  assert.deepEqual([...events].sort(), [...ACTIVITY_EVENTS].sort());
});

test("never records activity or touches the session itself", () => {
  for (const file of ["../src/components/devices/pairing.ts", "../src/views/DevicesView.vue"]) {
    const source = readFileSync(new URL(file, import.meta.url), "utf8").replace(/\/\/.*$|\/\*[\s\S]*?\*\//gm, "");
    for (const name of ["recordActivity", "sessionTouch", "session_touch"]) {
      assert.ok(!source.includes(name), `${file} must not use ${name}`);
    }
  }
});

test("stop (lock or unmount) cancels a scheduled refresh and removes the listeners", (t) => {
  const { calls, refresher, shell } = setup(t);
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell);
  refresher.stop(); // the lock that this same event caused unmounts the view first
  t.mock.timers.tick(0);
  assert.equal(calls.length, 0);
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell);
  refresher.onActivity();
  t.mock.timers.tick(0);
  assert.equal(calls.length, 0);
});

test("skips while the view is busy or a dialog is open, and retries on the next activity", (t) => {
  let ready = false;
  const { calls, shell } = setup(t, () => ready);
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 0);
  ready = true;
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 1);
});

test("one refresh at a time; a hanging one is dropped after the timeout", async (t) => {
  const { calls, shell } = setup(t);
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 1);
  t.mock.timers.tick(REFRESH_INTERVAL_MS);
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 1, "still running");
  t.mock.timers.tick(REFRESH_TIMEOUT_MS);
  await flush();
  activity(shell);
  t.mock.timers.tick(0);
  assert.equal(calls.length, 2);
});
