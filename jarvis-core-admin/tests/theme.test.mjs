import assert from "node:assert/strict";
import test from "node:test";
import { currentTheme, initTheme, resolveTheme, setTheme } from "../src/theme.ts";

function fakeEnv({ stored = null, prefersDark = false, storage = "ok" } = {}) {
  const values = new Map(stored === null ? [] : [["jarvis.theme", stored]]);
  const classes = new Set();
  const listeners = [];
  const media = { matches: prefersDark, addEventListener: (_type, fn) => listeners.push(fn) };
  const fail = () => { throw new Error("storage blocked"); };
  const localStorage = storage === "ok"
    ? { getItem: (k) => values.get(k) ?? null, setItem: (k, v) => values.set(k, v) }
    : { getItem: fail, setItem: fail };
  const root = { dataset: {}, offsetHeight: 0, classList: { add: (c) => classes.add(c), remove: (c) => classes.delete(c) } };
  return {
    env: { localStorage, matchMedia: () => media, document: { documentElement: root }, requestAnimationFrame: (fn) => fn() },
    root, values, media, classes, osChange: (dark) => { media.matches = dark; listeners.forEach((fn) => fn()); },
  };
}

test("system follows the OS preference; explicit choices win", () => {
  assert.equal(resolveTheme("system", false), "light");
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("dark", false), "dark");
});

test("unknown or missing stored values fall back to system", () => {
  assert.equal(currentTheme(fakeEnv().env), "system");
  assert.equal(currentTheme(fakeEnv({ stored: "dark" }).env), "dark");
  assert.equal(currentTheme(fakeEnv({ stored: "light" }).env), "light");
  assert.equal(currentTheme(fakeEnv({ stored: "neon" }).env), "system");
});

test("blocked storage never throws and still applies the choice", () => {
  const blocked = fakeEnv({ storage: "throws", prefersDark: true });
  assert.equal(currentTheme(blocked.env), "system");
  initTheme(blocked.env);
  assert.equal(blocked.root.dataset.theme, "dark");
  setTheme("light", blocked.env);
  assert.equal(blocked.root.dataset.theme, "light");
  blocked.osChange(true);
  assert.equal(blocked.root.dataset.theme, "light");
});

test("init paints the saved theme, setTheme persists, and system tracks OS changes", () => {
  const fake = fakeEnv({ stored: "system", prefersDark: false });
  initTheme(fake.env);
  assert.equal(fake.root.dataset.theme, "light");
  fake.osChange(true);
  assert.equal(fake.root.dataset.theme, "dark");
  setTheme("light", fake.env);
  assert.equal(fake.values.get("jarvis.theme"), "light");
  assert.equal(fake.root.dataset.theme, "light");
  fake.osChange(true);
  assert.equal(fake.root.dataset.theme, "light");
  assert.equal(fake.classes.has("no-tx"), false);
});
