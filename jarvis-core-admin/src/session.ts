import type { SessionStatus } from "./admin";

// The administrator session lock. The frontend lock is authoritative: after
// five minutes without pointer or keyboard activity the views unmount and the
// broker session is dropped. Pure helpers plus one thin wiring layer, with no
// Vue or Tauri imports, so tests load this file directly. The shell passes a
// reactive `state`, so every write here re-renders it.

export const IDLE_TIMEOUT_MS = 5 * 60 * 1000;
export const TOUCH_INTERVAL_MS = 5 * 1000;
export const IDLE_REASON = "Locked after five minutes without activity.";
export const ENDED_REASON = "The administrator session ended. Authenticate again.";

export interface SessionApi {
  sessionAuthenticate(): Promise<SessionStatus>;
  sessionTouch(): Promise<unknown>;
  sessionLock(): Promise<unknown>;
}
export interface SessionState { locked: boolean; authBusy: boolean; authError: string; lastActivity: number }

export const initialSession = (): SessionState => ({ locked: true, authBusy: false, authError: "", lastActivity: 0 });

/** True once IDLE_TIMEOUT_MS has passed since the last activity. */
export const idleExpired = (lastActivity: number, now: number) => now - lastActivity >= IDLE_TIMEOUT_MS;

/** "HH:MM" at which the idle lock fires. Derived from lastActivity; nothing ticks for it. */
export function lockTime(lastActivity: number): string {
  const at = new Date(lastActivity + IDLE_TIMEOUT_MS);
  return `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}

/** The view to render: none while locked, so a lock unmounts the view and its data. */
export const visibleView = <T>(locked: boolean, view: T): T | null => (locked ? null : view);

/** Navigation target: while locked the current view stays selected. */
export const navigate = <T>(locked: boolean, current: T, next: T): T => (locked ? current : next);

export function createSession(state: SessionState, api: SessionApi, errorText: (error: unknown) => string) {
  let idleTimer: ReturnType<typeof setInterval> | undefined;
  let lastBrokerTouch = 0;
  let touchPending = false;

  function lock(reason = IDLE_REASON) {
    if (state.locked) return;
    state.locked = true;
    state.authError = reason;
    void api.sessionLock().catch(() => undefined);
  }

  /** Pointer and keyboard activity inside the shell. Only DOM events call this, never a timer. */
  function recordActivity() {
    if (state.locked) return;
    const now = Date.now();
    if (idleExpired(state.lastActivity, now)) {
      lock();
      return;
    }
    state.lastActivity = now;
    if (touchPending || now - lastBrokerTouch < TOUCH_INTERVAL_MS) return;
    touchPending = true;
    lastBrokerTouch = now;
    void api.sessionTouch()
      .catch(() => lock(ENDED_REASON))
      .finally(() => { touchPending = false; });
  }

  function checkIdle() {
    if (state.locked) return;
    if (idleExpired(state.lastActivity, Date.now())) lock();
  }

  /** Resolves after authentication and `afterUnlock`; a failure leaves the session locked. */
  async function unlock(afterUnlock?: () => Promise<void>) {
    state.authBusy = true;
    state.authError = "";
    try {
      const status = await api.sessionAuthenticate();
      if (!status.authenticated) throw new Error("Administrator authentication did not complete.");
      state.locked = false;
      state.lastActivity = Date.now();
      lastBrokerTouch = state.lastActivity;
      await afterUnlock?.();
    } catch (error) {
      state.locked = true;
      state.authError = errorText(error);
    } finally {
      state.authBusy = false;
    }
  }

  function start() {
    idleTimer = setInterval(checkIdle, 1000);
  }

  /** Shell unmount: stop the idle check and drop the broker session. */
  function stop() {
    clearInterval(idleTimer);
    if (!state.locked) void api.sessionLock().catch(() => undefined);
  }

  return { lock, unlock, recordActivity, checkIdle, start, stop };
}
