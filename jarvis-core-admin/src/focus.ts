// Modal dialog focus: keep Tab inside, close on Escape, give focus back to the
// opener. Plain DOM, no Vue or Tauri imports, so tests load this file directly.

const FOCUSABLE = "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])";

/** Wraps Tab and Shift+Tab around the dialog's first and last focusable element. */
export function trapTab(event: KeyboardEvent, dialog: HTMLElement) {
  if (event.key !== "Tab") return;
  const items = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)];
  const active = dialog.ownerDocument.activeElement;
  // Focus on the dialog itself (tabindex -1) or outside it counts as a boundary.
  const outside = active === dialog || !dialog.contains(active);
  if (!items.length) {
    event.preventDefault();
    dialog.focus();
  } else if (event.shiftKey && (outside || active === items[0])) {
    event.preventDefault();
    items[items.length - 1].focus();
  } else if (!event.shiftKey && (outside || active === items[items.length - 1])) {
    event.preventDefault();
    items[0].focus();
  }
}

/**
 * Call when the dialog has mounted, before moving focus into it. Without
 * `onEscape` the dialog cannot be dismissed from the keyboard (the restart gate).
 */
export function dialogFocus(dialog: HTMLElement, onEscape?: () => void) {
  const opener = dialog.ownerDocument.activeElement as HTMLElement | null;
  return {
    keydown(event: KeyboardEvent) {
      if (event.key === "Escape" && onEscape) {
        event.preventDefault();
        onEscape();
      } else {
        trapTab(event, dialog);
      }
    },
    /** Back to the element that opened the dialog, if it is still on the page. */
    restore() {
      if (opener?.isConnected) opener.focus();
    },
  };
}
