// Sidebar navigation guard. One view at a time (Models & routing) can ask the
// shell to hold a sidebar move, for example to confirm dropping staged
// changes. No Vue or Tauri imports, so tests load this file directly.
import type { ViewName } from "./admin";

/** Asked before the sidebar leaves the current view: true lets it go now. */
export type LeaveGuard = (next: ViewName) => boolean;
export interface NavGuard {
  /** Install `guard` until the returned function removes it (on unmount). */
  set(guard: LeaveGuard): () => void;
  /**
   * Whether moving from `current` to `next` may happen now. Staying on the
   * same view never asks. While locked the guard is never asked: the lock
   * always wins, unmounts the guarding view and drops its staged changes.
   */
  allows(locked: boolean, current: ViewName, next: ViewName): boolean;
}
export const NAV_GUARD = "navGuard";

/** Leaving Models & routing: a running Apply blocks, staged changes ask first. */
export const leaveRouting = (applying: boolean, unsaved: number): "go" | "ask" | "block" =>
  applying ? "block" : unsaved ? "ask" : "go";

export function createNavGuard(): NavGuard {
  let active: LeaveGuard | undefined;
  return {
    set(guard) {
      active = guard;
      return () => { if (active === guard) active = undefined; };
    },
    allows(locked, current, next) {
      return locked || current === next || !active || active(next);
    },
  };
}
