import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { REFRESH_TIMEOUT_MS } from "../src/components/devices/pairing.ts";
import { FOLLOW_IDLE_MS, FOLLOW_INTERVAL_MS, LOG_SERVICES, createFollow, pickService } from "../src/components/logs/logs.ts";

const flush = () => new Promise((resolve) => setImmediate(resolve));
const source = (file) => readFileSync(new URL(file, import.meta.url), "utf8");

test("the preselect accepts exactly the seven allowlisted services, Core otherwise", () => {
  const admin = source("../src/admin.ts");
  const allowlist = [...admin.match(/export type LogService =([^;]+);/)[1].matchAll(/"([a-z-]+)"/g)].map((match) => match[1]);
  assert.equal(allowlist.length, 7);
  assert.deepEqual(LOG_SERVICES.map((item) => item.value), allowlist);
  for (const service of allowlist) assert.equal(pickService(service), service);
  for (const value of [undefined, null, "", "caddy", "jarvis-core", "CORE", " core", "core ", "../core", "surrealdb\n", 1, {}, ["core"]]) {
    assert.equal(pickService(value), "core", `${JSON.stringify(value)} falls back to Core`);
  }
});

// A follower on a fake shell, with mocked clock and timers.
function setup(t) {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval", "Date"], now: 1_000_000 });
  const calls = [];
  const state = { allowed: true, idle: 0, resolve: () => {} };
  const follower = createFollow(() => {
    calls.push(Date.now());
    return new Promise((resolve) => { state.resolve = resolve; });
  }, () => state.allowed, () => { state.idle += 1; });
  const shell = new EventTarget();
  follower.listen(shell);
  t.after(() => follower.dispose());
  return { calls, state, follower, shell, finish: async () => { state.resolve(); await flush(); } };
}

test("polls at once, then every 2.5 s while followed, one request at a time", async (t) => {
  const { calls, follower, finish } = setup(t);
  assert.equal(FOLLOW_INTERVAL_MS, 2500);
  follower.start();
  assert.equal(follower.following, true);
  assert.equal(calls.length, 1, "Follow reads the log right away");
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 1, "the first poll has not answered yet");
  await finish();
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 2);
  await finish();
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 3);
});

test("a hanging poll is dropped after the timeout", async (t) => {
  const { calls, follower, shell } = setup(t);
  follower.start();
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 1);
  shell.dispatchEvent(new Event("pointermove"));
  t.mock.timers.tick(REFRESH_TIMEOUT_MS);
  await flush();
  shell.dispatchEvent(new Event("pointermove"));
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 2);
});

test("pauses after 60 s without shell activity and stops polling", async (t) => {
  const { calls, state, follower, finish } = setup(t);
  assert.equal(FOLLOW_IDLE_MS, 60_000);
  follower.start();
  for (let at = 0; at < FOLLOW_IDLE_MS - FOLLOW_INTERVAL_MS; at += FOLLOW_INTERVAL_MS) {
    t.mock.timers.tick(FOLLOW_INTERVAL_MS);
    await finish();
  }
  const polled = calls.length;
  assert.equal(state.idle, 0);
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(state.idle, 1, "paused at 60 s");
  assert.equal(follower.following, false);
  assert.equal(calls.length, polled, "the pausing tick sends nothing");
  t.mock.timers.tick(10 * 60 * 1000);
  assert.equal(calls.length, polled, "nothing after the pause");
  assert.equal(state.idle, 1);
});

test("the shell's activity events keep Follow going; its own polls never count", async (t) => {
  const { calls, state, follower, shell, finish } = setup(t);
  follower.start();
  for (let i = 0; i < 60; i++) {
    t.mock.timers.tick(FOLLOW_INTERVAL_MS);
    await finish();
    if (i % 10 === 0) shell.dispatchEvent(new Event(i % 20 ? "keydown" : "wheel"));
  }
  assert.equal(state.idle, 0, "150 s with activity every 25 s");
  assert.ok(calls.length > 50);
  t.mock.timers.tick(FOLLOW_IDLE_MS);
  await flush();
  assert.equal(state.idle, 1, "polls answering do not count as activity");
});

test("stops without polling once locked or unmounted", async (t) => {
  const { calls, state, follower, finish } = setup(t);
  follower.start();
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  await finish();
  state.allowed = false; // locked
  t.mock.timers.tick(FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 1);
  assert.equal(follower.following, false);
  state.allowed = true;
  t.mock.timers.tick(10 * FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 1, "a stopped follower stays stopped");
  assert.equal(state.idle, 0, "a lock is not reported as an idle pause");

  state.allowed = false;
  follower.start();
  assert.equal(follower.following, false, "cannot start while locked");
});

test("dispose (unmount) cancels the timer and removes the activity listeners", (t) => {
  const { calls, follower, shell } = setup(t);
  const removed = [];
  const remove = shell.removeEventListener.bind(shell);
  shell.removeEventListener = (name, ...rest) => { removed.push(name); return remove(name, ...rest); };
  follower.start();
  assert.equal(calls.length, 1);
  follower.dispose();
  t.mock.timers.tick(10 * FOLLOW_INTERVAL_MS);
  assert.equal(calls.length, 1, "nothing after unmount");
  assert.deepEqual(removed.sort(), ["focusin", "keydown", "mouseenter", "pointerdown", "pointermove", "touchstart", "wheel"]);
});

test("Logs never records activity or touches the session itself", () => {
  for (const file of ["../src/components/logs/logs.ts", "../src/views/LogsView.vue"]) {
    const code = source(file).replace(/\/\/.*$|\/\*[\s\S]*?\*\//gm, "");
    for (const name of ["recordActivity", "sessionTouch", "session_touch"]) {
      assert.ok(!code.includes(name), `${file} must not use ${name}`);
    }
  }
});

test("the log list is not a live region; a separate status line is", () => {
  const view = source("../src/views/LogsView.vue");
  const list = view.match(/<section ref="viewport"[^>]*>/)[0];
  assert.ok(!/aria-live|role="(status|log|alert)"/.test(list), list);
  assert.match(view, /<p class="log-status" role="status">/);
  assert.match(view, /:aria-pressed="follow"/);
});

test("the shell blocks Follow while locked and behind the restart gate", () => {
  const app = source("../src/App.vue");
  assert.match(app, /provide\("pollingBlocked", \(\) => session\.locked \|\| restartRequired\.value\);/);
  assert.match(source("../src/views/LogsView.vue"), /createFollow\(\(\) => load\(true\), \(\) => mounted && !pollingBlocked\(\),/);
});
