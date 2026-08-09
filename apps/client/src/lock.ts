// App-lock: the desktop app sits behind a biometric gate (Touch ID / Face ID).
//
// Primary unlock is local biometrics (biometrics-only — no desktop-password
// fallback on purpose). If that fails, the app falls back to approval from a
// trusted phone (see unlock-via-phone in `phoneUnlock`). The phone itself is
// never locked in-app: it is the approver and is already device-locked.
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

export const locked = ref(false); // resolved by init() — desktop starts locked
export const unlocking = ref(false);
export const lockError = ref<string | null>(null);
export const isDesktop = ref(false);

// Auto re-lock after this much inactivity (ms). 0 disables it.
const INACTIVITY_MS = 5 * 60 * 1000;
let idleTimer: number | undefined;

/** Decide whether this platform locks in-app (desktop yes, phone no). */
export async function initLock(): Promise<void> {
  try {
    const info = await invoke<{ platform: string }>("device_info");
    isDesktop.value = ["macos", "windows", "linux"].includes(info.platform);
  } catch {
    isDesktop.value = false;
  }
  locked.value = isDesktop.value;
}

export function unlockApp(): void {
  locked.value = false;
  lockError.value = null;
  armIdle();
}

export function lockApp(): void {
  if (isDesktop.value) locked.value = true;
}

/** Local biometric unlock (Touch ID / Face ID). Returns whether it succeeded. */
export async function biometricUnlock(): Promise<boolean> {
  if (unlocking.value) return false;
  unlocking.value = true;
  lockError.value = null;
  try {
    await invoke("biometric_unlock", { reason: "Jarvis ontgrendelen" });
    unlockApp();
    return true;
  } catch (e) {
    lockError.value = e instanceof Error ? e.message : String(e);
    return false;
  } finally {
    unlocking.value = false;
  }
}

function armIdle(): void {
  if (!INACTIVITY_MS || !isDesktop.value) return;
  clearTimeout(idleTimer);
  idleTimer = window.setTimeout(() => lockApp(), INACTIVITY_MS);
}

/** Reset the inactivity timer on user interaction while unlocked. */
export function noteActivity(): void {
  if (!locked.value) armIdle();
}
