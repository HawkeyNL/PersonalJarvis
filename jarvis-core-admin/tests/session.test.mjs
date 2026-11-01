import assert from "node:assert/strict";
import test from "node:test";
import {
  ENDED_REASON, IDLE_REASON, IDLE_TIMEOUT_MS, createSession, idleExpired, initialSession, lockTime, navigate, visibleView,
} from "../src/session.ts";

const flush = () => new Promise((resolve) => setImmediate(resolve));

function fakeApi({ authenticated = true, touchFails = false } = {}) {
  const calls = { authenticate: 0, touch: 0, lock: 0 };
  return {
    calls,
    api: {
      sessionAuthenticate: async () => { calls.authenticate += 1; return { authenticated, expires_in_seconds: 300 }; },
      sessionTouch: async () => { calls.touch += 1; if (touchFails) throw new Error("expired"); return {}; },
      sessionLock: async () => { calls.lock += 1; return {}; },
    },
  };
}

// An unlocked session with the idle timer running on mocked time.
async function unlockedSession(t, options) {
  t.mock.timers.enable({ apis: ["setInterval", "Date"], now: 1_000_000 });
  const state = initialSession();
  const { api, calls } = fakeApi(options);
  const session = createSession(state, api, (error) => error.message);
  session.start();
  await session.unlock();
  t.after(() => session.stop());
  return { state, calls, session };
}

test("starts locked and unlocks only after authentication", async (t) => {
  const { state, calls } = await unlockedSession(t);
  assert.equal(calls.authenticate, 1);
  assert.equal(state.locked, false);
  assert.equal(state.authBusy, false);
  assert.equal(state.lastActivity, 1_000_000);
  assert.equal(initialSession().locked, true);
});

test("a refused authentication stays locked and runs no unlock hook", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: 0 });
  const state = initialSession();
  const session = createSession(state, fakeApi({ authenticated: false }).api, (error) => error.message);
  let hook = 0;
  await session.unlock(async () => { hook += 1; });
  assert.equal(state.locked, true);
  assert.equal(hook, 0);
  assert.match(state.authError, /did not complete/);
});

test("locks after five minutes without activity", async (t) => {
  const { state, calls } = await unlockedSession(t);
  t.mock.timers.tick(IDLE_TIMEOUT_MS - 1000);
  assert.equal(state.locked, false);
  t.mock.timers.tick(1000);
  assert.equal(state.locked, true);
  assert.equal(state.authError, IDLE_REASON);
  assert.equal(calls.lock, 1);
});

test("activity resets the idle timer", async (t) => {
  const { state, session } = await unlockedSession(t);
  t.mock.timers.tick(4 * 60 * 1000);
  session.recordActivity();
  t.mock.timers.tick(4 * 60 * 1000);
  assert.equal(state.locked, false, "eight minutes in, four after the last activity");
  t.mock.timers.tick(60 * 1000);
  assert.equal(state.locked, true);
});

test("activity after the timeout locks instead of renewing", async (t) => {
  const { state, session } = await unlockedSession(t);
  // The idle check has not run yet (for example after a suspend); the next event must not revive the session.
  t.mock.timers.setTime(Date.now() + IDLE_TIMEOUT_MS);
  session.recordActivity();
  assert.equal(state.locked, true);
});

test("broker touches are throttled and a failed touch locks", async (t) => {
  const { state, calls, session } = await unlockedSession(t, { touchFails: true });
  session.recordActivity();
  assert.equal(calls.touch, 0, "unlock counts as the first touch");
  t.mock.timers.tick(5000);
  session.recordActivity();
  session.recordActivity();
  assert.equal(calls.touch, 1);
  await flush();
  assert.equal(state.locked, true);
  assert.equal(state.authError, ENDED_REASON);
});

test("a locked session ignores activity and the idle check", async (t) => {
  const { state, calls, session } = await unlockedSession(t);
  session.lock("Locked by owner.");
  assert.equal(state.authError, "Locked by owner.");
  const last = state.lastActivity;
  t.mock.timers.tick(5000);
  session.recordActivity();
  assert.equal(state.lastActivity, last, "activity does not renew a locked session");
  t.mock.timers.tick(IDLE_TIMEOUT_MS);
  assert.equal(calls.lock, 1);
  assert.equal(calls.touch, 0);
  assert.equal(state.locked, true);
});

// The helper contract App.vue relies on (`navigate` for every entry and tab,
// `<component v-if="current">` with `visibleView`). The rendered result, with
// disabled entries and no mounted view, was checked in headless Chrome.
test("locked state disables navigation and unmounts the view", () => {
  assert.equal(navigate(true, "overview", "devices"), "overview");
  assert.equal(navigate(false, "overview", "devices"), "devices");
  const view = { name: "DevicesView" };
  assert.equal(visibleView(true, view), null);
  assert.equal(visibleView(false, view), view);
});

test("unmount drops an unlocked session and stops the idle timer", async (t) => {
  const { state, calls, session } = await unlockedSession(t);
  session.stop();
  assert.equal(calls.lock, 1);
  t.mock.timers.tick(IDLE_TIMEOUT_MS);
  assert.equal(state.locked, false, "no idle check runs after unmount");
  assert.equal(calls.lock, 1);
});

test("unmount of a locked session sends no second lock", async (t) => {
  const { calls, session } = await unlockedSession(t);
  session.lock();
  session.stop();
  assert.equal(calls.lock, 1);
});

test("the lock time is derived from the last activity", () => {
  assert.equal(idleExpired(0, IDLE_TIMEOUT_MS - 1), false);
  assert.equal(idleExpired(0, IDLE_TIMEOUT_MS), true);
  const last = new Date(2026, 10, 1, 14, 58, 30).getTime();
  assert.equal(lockTime(last), "15:03");
  assert.equal(lockTime(new Date(2026, 10, 1, 23, 57, 0).getTime()), "00:02");
});
