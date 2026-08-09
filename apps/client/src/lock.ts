// App-lock: the desktop app sits behind a biometric gate (Touch ID / Face ID).
//
// Primary unlock is local biometrics (biometrics-only — no desktop-password
// fallback on purpose). If that fails, the app falls back to approval from a
// trusted phone: the desktop posts an unlock request and polls until the phone
// signs it (see `requestPhoneApproval`). The phone itself is never locked
// in-app: it is the approver and is already device-locked.
//
// The lock is opt-in via a Settings toggle (off by default) so day-to-day
// development isn't interrupted by prompts.
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { currentSession } from "./auth";
import { getJsonAuth, postJsonAuth } from "./api";

export const locked = ref(false); // resolved by initLock()
export const unlocking = ref(false);
export const lockError = ref<string | null>(null);
export const isDesktop = ref(false);

// Phone-approval state, for the lock screen to reflect.
export const phoneWaiting = ref(false);
export const phoneError = ref<string | null>(null);

const LKEY = "jarvis.lock.enabled";
export const lockEnabled = ref(localStorage.getItem(LKEY) === "true");

// Auto re-lock after this much inactivity (ms). 0 disables it.
const INACTIVITY_MS = 5 * 60 * 1000;
let idleTimer: number | undefined;
let pollTimer: number | undefined;

/** Decide whether this platform locks in-app (desktop yes, phone no). */
export async function initLock(): Promise<void> {
  try {
    const info = await invoke<{ platform: string }>("device_info");
    isDesktop.value = ["macos", "windows", "linux"].includes(info.platform);
  } catch {
    isDesktop.value = false;
  }
  locked.value = isDesktop.value && lockEnabled.value;
}

export function setLockEnabled(v: boolean): void {
  lockEnabled.value = v;
  localStorage.setItem(LKEY, String(v));
  if (!v) unlockApp();
  else if (isDesktop.value) locked.value = true;
}

export function unlockApp(): void {
  stopPhonePoll();
  locked.value = false;
  lockError.value = null;
  phoneError.value = null;
  armIdle();
}

export function lockApp(): void {
  if (isDesktop.value && lockEnabled.value) locked.value = true;
}

/** Local biometric unlock (Touch ID / Face ID), biometrics-only. */
export async function biometricUnlock(): Promise<boolean> {
  if (unlocking.value) return false;
  unlocking.value = true;
  lockError.value = null;
  try {
    await invoke("biometric_unlock", { reason: "Jarvis ontgrendelen", allowPassword: false });
    unlockApp();
    return true;
  } catch (e) {
    lockError.value = e instanceof Error ? e.message : String(e);
    return false;
  } finally {
    unlocking.value = false;
  }
}

/** Ask a trusted phone to approve the unlock; poll until it signs (or expires). */
export async function requestPhoneApproval(): Promise<void> {
  phoneError.value = null;
  const session = await currentSession();
  const token = session.token;
  if (!token) {
    phoneError.value = "niet ingelogd";
    return;
  }
  try {
    const { request_id } = await postJsonAuth<{ request_id: string; nonce: string }>(
      "/v1/auth/unlock/request",
      token,
      {},
    );
    phoneWaiting.value = true;
    const deadline = Date.now() + 2 * 60 * 1000;
    clearInterval(pollTimer);
    pollTimer = window.setInterval(async () => {
      if (Date.now() > deadline) {
        stopPhonePoll();
        phoneError.value = "verzoek verlopen — probeer opnieuw";
        return;
      }
      try {
        const { status } = await getJsonAuth<{ status: string }>(
          `/v1/auth/unlock/${request_id}`,
          token,
        );
        if (status === "approved") unlockApp();
        else if (status === "expired" || status === "denied") {
          stopPhonePoll();
          phoneError.value = "verzoek verlopen — probeer opnieuw";
        }
      } catch {
        /* transient network error — keep polling until the deadline */
      }
    }, 2000);
  } catch (e) {
    phoneError.value = e instanceof Error ? e.message : String(e);
  }
}

export function cancelPhoneApproval(): void {
  stopPhonePoll();
  phoneError.value = null;
}

function stopPhonePoll(): void {
  clearInterval(pollTimer);
  pollTimer = undefined;
  phoneWaiting.value = false;
}

function armIdle(): void {
  if (!INACTIVITY_MS || !isDesktop.value || !lockEnabled.value) return;
  clearTimeout(idleTimer);
  idleTimer = window.setTimeout(() => lockApp(), INACTIVITY_MS);
}

/** Reset the inactivity timer on user interaction while unlocked. */
export function noteActivity(): void {
  if (!locked.value) armIdle();
}
