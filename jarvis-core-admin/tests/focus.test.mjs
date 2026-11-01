import assert from "node:assert/strict";
import test from "node:test";
import { dialogFocus, trapTab } from "../src/focus.ts";

// Just enough DOM for the focus helpers: elements that take focus, a dialog
// that lists its focusable children, and one document tracking activeElement.
function fakeDom(names) {
  const doc = { activeElement: null };
  const element = (name) => ({ name, isConnected: true, disabled: false, ownerDocument: doc, focus() { if (!this.disabled) doc.activeElement = this; }, matches: function () { return this.disabled; } });
  const page = element("page");
  const opener = { ...element("opener"), closest: () => page };
  const items = names.map(element);
  const dialog = { ...element("dialog"), querySelectorAll: () => items, contains: (node) => node === dialog || items.includes(node) };
  dialog.focus = () => { doc.activeElement = dialog; };
  opener.focus();
  return { doc, opener, page, items, dialog };
}
const key = (name, shiftKey = false) => ({ key: name, shiftKey, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } });

test("Tab wraps from the last element to the first, Shift+Tab the other way", () => {
  const { doc, items: [cancel, confirm], dialog } = fakeDom(["cancel", "confirm"]);
  confirm.focus();
  const tab = key("Tab");
  trapTab(tab, dialog);
  assert.equal(doc.activeElement, cancel);
  assert.ok(tab.defaultPrevented);
  const back = key("Tab", true);
  trapTab(back, dialog);
  assert.equal(doc.activeElement, confirm);
  assert.ok(back.defaultPrevented);
});

test("Tab between inner elements is left to the browser", () => {
  const { doc, items: [cancel], dialog } = fakeDom(["cancel", "confirm"]);
  cancel.focus();
  const tab = key("Tab");
  trapTab(tab, dialog);
  assert.equal(doc.activeElement, cancel);
  assert.equal(tab.defaultPrevented, false);
});

test("focus on the dialog itself or outside it is pulled back in", () => {
  const { doc, opener, items: [cancel, confirm], dialog } = fakeDom(["cancel", "confirm"]);
  dialog.focus();
  trapTab(key("Tab", true), dialog);
  assert.equal(doc.activeElement, confirm);
  opener.focus();
  trapTab(key("Tab"), dialog);
  assert.equal(doc.activeElement, cancel);
});

test("without focusable elements Tab keeps focus on the dialog", () => {
  const { doc, dialog } = fakeDom([]);
  const tab = key("Tab");
  trapTab(tab, dialog);
  assert.equal(doc.activeElement, dialog);
  assert.ok(tab.defaultPrevented);
});

test("Escape cancels; other keys do not", () => {
  const { items: [cancel], dialog } = fakeDom(["cancel"]);
  let cancelled = 0;
  const focus = dialogFocus(dialog, () => { cancelled += 1; });
  cancel.focus();
  focus.keydown(key("Enter"));
  assert.equal(cancelled, 0);
  const escape = key("Escape");
  focus.keydown(escape);
  assert.equal(cancelled, 1);
  assert.ok(escape.defaultPrevented);
});

test("a dialog without onEscape (the restart gate) moves no focus, ignores Escape and traps Tab", () => {
  const { doc, opener, items: [restart], dialog } = fakeDom(["restart"]);
  const focus = dialogFocus(dialog);
  // No programmatic focus: a focusin would count as activity for the idle lock.
  assert.equal(doc.activeElement, opener);
  restart.focus();
  const escape = key("Escape");
  focus.keydown(escape);
  assert.equal(escape.defaultPrevented, false);
  focus.keydown(key("Tab"));
  assert.equal(doc.activeElement, restart);
});

test("closing gives focus back to the opener, unless it left the page", () => {
  const { doc, opener, items: [cancel], dialog } = fakeDom(["cancel"]);
  const focus = dialogFocus(dialog, () => {});
  cancel.focus();
  focus.restore();
  assert.equal(doc.activeElement, opener);
  cancel.focus();
  opener.isConnected = false;
  focus.restore();
  assert.equal(doc.activeElement, cancel);
});

test("an opener disabled by the work it started hands focus to its page", () => {
  const { doc, opener, page, items: [confirm], dialog } = fakeDom(["confirm"]);
  const focus = dialogFocus(dialog, () => {});
  confirm.focus();
  opener.disabled = true;
  focus.restore();
  assert.equal(doc.activeElement, page);
});
