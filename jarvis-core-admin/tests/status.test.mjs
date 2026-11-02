import assert from "node:assert/strict";
import test from "node:test";
import { fieldLabel, keyedState, stateLabel, toneOf } from "../src/status.ts";

test("intended and not-yet-run states are neutral, not red or amber", () => {
  for (const state of ["disabled", "manual", "off", "not run this session", "logged_out"]) assert.equal(toneOf(state), "idle", state);
  assert.equal(toneOf("Disabled"), "idle");
});

test("healthy states are ok and failures stay red", () => {
  for (const state of ["active", "enabled", "passed", "connected", "automatic", "up to date", "available"]) assert.equal(toneOf(state), "ok", state);
  for (const state of ["failed", "inactive", "unavailable", "error", "unhealthy"]) assert.equal(toneOf(state), "error", state);
  assert.equal(toneOf("update available"), "warn");
  assert.equal(toneOf("incompatible_runtime"), "warn");
});

test("a key gives its value meaning: manual updater, up-to-date check", () => {
  assert.equal(keyedState("updater", "disabled"), "manual");
  assert.equal(keyedState("Updater", "enabled"), "automatic");
  assert.equal(keyedState("update", "not available"), "up to date");
  assert.equal(keyedState("update", "repair required"), "repair required");
  assert.equal(keyedState("jarvis-core.service", "disabled"), "disabled");
  assert.equal(toneOf(keyedState("updater", "disabled")), "idle");
  assert.equal(stateLabel(keyedState("updater", "disabled")), "Manual");
});

test("raw CLI keys and states get human labels", () => {
  assert.equal(fieldLabel("core_app_latest"), "Latest admin app");
  assert.equal(fieldLabel("systemd_units"), "Service units");
  assert.equal(fieldLabel("some_new_key"), "Some new key");
  assert.equal(stateLabel("logged_out"), "Signed out");
  assert.equal(stateLabel("socket_ready"), "Ready, starts on demand");
  assert.equal(stateLabel("not run this session"), "Not run yet");
  assert.equal(stateLabel("release-matched"), "Matches release");
  assert.equal(stateLabel("active"), "Active");
  assert.equal(stateLabel("migration required"), "Migration required");
});

test("versions and identifiers pass through unchanged", () => {
  assert.equal(stateLabel("v0.0.71"), "v0.0.71");
  assert.equal(stateLabel("0.1.38"), "0.1.38");
  assert.equal(stateLabel("main@8a99157"), "main@8a99157");
});

test("an unset key is neutral; account billing and runtime lines read as words", () => {
  assert.equal(toneOf("not configured"), "idle");
  assert.equal(stateLabel("not configured"), "Not set");
  assert.equal(stateLabel(keyedState("billing", "subscription")), "Subscription login");
  assert.equal(stateLabel(keyedState("billing", "overage_unverified")), "Extra usage setting not verified");
  assert.equal(stateLabel(keyedState("runtime", "inactive")), "Worker socket off");
  assert.equal(stateLabel(keyedState("runtime", "active")), "Worker running");
  assert.equal(stateLabel(keyedState("runtime", "socket_ready")), "Ready, starts on demand");
});
