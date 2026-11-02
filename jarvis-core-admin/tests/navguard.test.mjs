import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createNavGuard, leaveRouting } from "../src/navguard.ts";
import { navigate } from "../src/session.ts";

// The guard ModelsView installs (leaveRouting), recording what it would show.
function models() {
  const state = { unsaved: 0, applying: false, asked: [], blocked: 0 };
  const guard = (next) => {
    const decision = leaveRouting(state.applying, state.unsaved);
    if (decision === "block") state.blocked += 1;
    if (decision === "ask") state.asked.push(next);
    return decision === "go";
  };
  return { state, guard };
}

// The shell's go(): the guard first, then the lock rule from session.ts.
const go = (nav, locked, current, next) => (nav.allows(locked, current, next) ? navigate(locked, current, next) : current);

test("leaving routing: Apply running blocks, staged changes ask, otherwise go", () => {
  assert.equal(leaveRouting(false, 0), "go");
  assert.equal(leaveRouting(false, 2), "ask");
  assert.equal(leaveRouting(true, 2), "block");
  assert.equal(leaveRouting(true, 0), "block");
});

test("without a guard, or with nothing staged, the sidebar moves freely", () => {
  const nav = createNavGuard();
  assert.equal(go(nav, false, "models", "logs"), "logs");
  nav.set(models().guard);
  assert.equal(go(nav, false, "models", "usage"), "usage");
});

test("staged changes hold the move and ask, naming the target", () => {
  const nav = createNavGuard();
  const { state, guard } = models();
  nav.set(guard);
  state.unsaved = 2;
  assert.equal(go(nav, false, "models", "logs"), "models");
  assert.deepEqual(state.asked, ["logs"]);
  // Discard: the view clears its count, then navigates.
  state.unsaved = 0;
  assert.equal(go(nav, false, "models", "logs"), "logs");
});

test("a running Apply blocks the move without asking", () => {
  const nav = createNavGuard();
  const { state, guard } = models();
  nav.set(guard);
  state.unsaved = 3;
  state.applying = true;
  assert.equal(go(nav, false, "models", "overview"), "models");
  assert.equal(state.blocked, 1);
  assert.deepEqual(state.asked, []);
  state.applying = false;
  state.unsaved = 0;
  assert.equal(go(nav, false, "models", "overview"), "overview");
});

test("staying on the same view never asks", () => {
  const nav = createNavGuard();
  const { state, guard } = models();
  nav.set(guard);
  state.unsaved = 1;
  assert.equal(go(nav, false, "models", "models"), "models");
  assert.deepEqual(state.asked, []);
});

test("the lock always wins: the guard is never asked while locked", () => {
  const nav = createNavGuard();
  const { state, guard } = models();
  nav.set(guard);
  state.unsaved = 1;
  state.applying = true;
  assert.equal(nav.allows(true, "models", "logs"), true);
  assert.equal(state.blocked, 0);
  assert.deepEqual(state.asked, []);
  // While locked, session.ts keeps the current view selected.
  assert.equal(go(nav, true, "models", "logs"), "models");
});

test("unmounting removes the guard; a stale remover leaves a newer guard alone", () => {
  const nav = createNavGuard();
  const first = models();
  const removeFirst = nav.set(first.guard);
  first.state.unsaved = 1;
  removeFirst();
  assert.equal(go(nav, false, "models", "logs"), "logs", "a lock unmounts Models: its staged changes no longer hold anything");

  const second = models();
  nav.set(second.guard);
  second.state.unsaved = 1;
  removeFirst();
  assert.equal(go(nav, false, "models", "logs"), "models");
});

test("the shell asks the guard on every sidebar move, and Models removes it on unmount", () => {
  const app = readFileSync(new URL("../src/App.vue", import.meta.url), "utf8");
  const body = app.match(/function go\(view: ViewName\) \{([\s\S]*?)\n\}/)[1];
  assert.match(body, /if \(!navGuard\.allows\(session\.locked, active\.value, view\)\) return;/);
  const view = readFileSync(new URL("../src/views/ModelsView.vue", import.meta.url), "utf8");
  assert.match(view, /leaveRouting\(applying\.value, unsaved\.value\)/);
  assert.match(view, /onBeforeUnmount\(\(\) => removeGuard\?\.\(\)\)/);
});
