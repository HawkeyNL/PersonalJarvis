// Pairing helpers for the Devices view: fingerprint display, expiry countdown
// and the activity-driven refresh of the pending list. No Vue or Tauri
// imports, so `npm run test:unit` loads this file directly.

/** The full key fingerprint in groups of four, exactly as the Jarvis apps show it. */
export const groupFingerprint = (fingerprint: string): string => fingerprint.match(/.{1,4}/g)?.join(" ") ?? "";

/** Seconds until `expiresAt` (Unix seconds), never negative. */
export const secondsLeft = (expiresAt: number, now: number): number => Math.max(0, Math.ceil(expiresAt - now / 1000));

export const isExpired = (expiresAt: number, now: number): boolean => secondsLeft(expiresAt, now) === 0;

/** "4:05" until expiry, "Expired" after. */
export function countdown(expiresAt: number, now: number): string {
  const left = secondsLeft(expiresAt, now);
  return left ? `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` : "Expired";
}

// ponytail: Core creates pairing requests with `expires_at = time::now() + 5m`
// (crates/identity/src/surreal.rs) and devices.rs does not pass `created_at`
// through. Pass it through when the Rust side next changes.
export const PAIRING_LIFETIME_S = 5 * 60;
export const requestedAt = (expiresAt: number): Date => new Date((expiresAt - PAIRING_LIFETIME_S) * 1000);

export const REFRESH_INTERVAL_MS = 15_000;
export const REFRESH_TIMEOUT_MS = 20_000;
/** The events App.vue's shell counts as activity for the idle lock. */
export const ACTIVITY_EVENTS = ["pointermove", "pointerdown", "mouseenter", "wheel", "touchstart", "keydown", "focusin"] as const;

/** Rejects when `promise` takes longer than `ms`; the timer never outlives it. */
export function withTimeout<T>(promise: Promise<T>, ms: number, message = "The Home Node did not answer in time."): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error(message)), ms); });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * Refreshes the pending list on the shell's own activity events, at most once
 * per REFRESH_INTERVAL_MS, never on a timer. Every broker request resets the
 * backend session window, so a timed poll would keep the root broker alive
 * after the user left. Here each refresh starts in the same moment as an event
 * the shell already records as activity (it never calls recordActivity
 * itself), so it cannot move the backend window past the frontend idle lock.
 * No activity means no refresh. The view only exists while unlocked; a lock
 * unmounts it and `stop` cancels a refresh that has not started yet.
 * `ready` lets the view skip a refresh while it is busy or a dialog is open.
 */
export function createActivityRefresh(refresh: () => Promise<unknown>, ready: () => boolean = () => true) {
  let last = Date.now(); // the view loads on mount
  let pending: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let target: EventTarget | undefined;

  function onActivity() {
    if (!target || running || pending || Date.now() - last < REFRESH_INTERVAL_MS) return;
    // Deferred one task: a lock caused by this same event unmounts the view
    // (a microtask) first, and that cancels this refresh.
    pending = setTimeout(() => {
      pending = undefined;
      if (!target || !ready()) return;
      last = Date.now();
      running = true;
      void withTimeout(refresh(), REFRESH_TIMEOUT_MS).catch(() => undefined).finally(() => { running = false; });
    });
  }

  return {
    onActivity,
    start(on: EventTarget) {
      target = on;
      for (const name of ACTIVITY_EVENTS) on.addEventListener(name, onActivity, { passive: true });
    },
    stop() {
      clearTimeout(pending);
      pending = undefined;
      for (const name of ACTIVITY_EVENTS) target?.removeEventListener(name, onActivity);
      target = undefined;
    },
    /** A manual or post-action load also counts as the latest refresh. */
    refreshed() { last = Date.now(); },
  };
}
