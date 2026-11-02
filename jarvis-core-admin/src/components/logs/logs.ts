// Logs view helpers: the fixed service allowlist and Follow polling. No Vue or
// Tauri imports, so `npm run test:unit` loads this file directly.
import type { LogService } from "../../admin";
import { ACTIVITY_EVENTS, REFRESH_TIMEOUT_MS, withTimeout } from "../devices/pairing.ts";

/** The fixed journal allowlist the broker accepts (admin.ts LogService). */
export const LOG_SERVICES: { value: LogService; label: string }[] = [
  { value: "core", label: "Core" }, { value: "surrealdb", label: "SurrealDB" },
  { value: "config-broker", label: "Config broker" }, { value: "codex-broker", label: "Codex broker" },
  { value: "opensandbox", label: "OpenSandbox" }, { value: "updater", label: "Updater" },
  { value: "agents-updater", label: "Agent updater" },
];

/** A preselected service, only when it is on the allowlist; Core otherwise. */
export const pickService = (value: unknown): LogService =>
  LOG_SERVICES.find((item) => item.value === value)?.value ?? "core";

export const FOLLOW_INTERVAL_MS = 2500;
export const FOLLOW_IDLE_MS = 60_000;

/**
 * Follow: poll the log every FOLLOW_INTERVAL_MS, one request at a time.
 * Every broker request resets the backend session window, so polling must
 * never keep a session alive on its own:
 * - it never calls the shell's recordActivity, so the frontend idle lock
 *   stays authoritative;
 * - it pauses after FOLLOW_IDLE_MS without the shell's own activity events
 *   and calls `onIdle`;
 * - a tick while `allowed()` is false (locked, unmounted) stops it without
 *   polling;
 * - `stop` cancels the timer; a request already sent is not repeated.
 */
export function createFollow(poll: () => Promise<unknown>, allowed: () => boolean, onIdle: () => void) {
  let timer: ReturnType<typeof setInterval> | undefined;
  let lastActivity = 0;
  let running = false;
  let target: EventTarget | undefined;
  const activity = () => { lastActivity = Date.now(); };

  function stop() {
    clearInterval(timer);
    timer = undefined;
  }
  function tick() {
    if (!allowed()) return stop();
    if (Date.now() - lastActivity >= FOLLOW_IDLE_MS) {
      stop();
      return onIdle();
    }
    if (running) return;
    running = true;
    void withTimeout(poll(), REFRESH_TIMEOUT_MS).catch(() => undefined).finally(() => { running = false; });
  }

  return {
    /** Starting counts as activity: the owner just pressed Follow. */
    start() {
      stop();
      if (!allowed()) return;
      activity();
      timer = setInterval(tick, FOLLOW_INTERVAL_MS);
    },
    stop,
    get following() { return timer !== undefined; },
    /** Listen to the shell's activity events, the ones the idle lock counts. */
    listen(on: EventTarget) {
      target = on;
      for (const name of ACTIVITY_EVENTS) on.addEventListener(name, activity, { passive: true });
    },
    dispose() {
      stop();
      for (const name of ACTIVITY_EVENTS) target?.removeEventListener(name, activity);
      target = undefined;
    },
  };
}
