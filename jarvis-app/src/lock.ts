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
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { currentAuthStatus } from "./auth";
import { getJsonAuth, postJsonAuth } from "./api";
import {
  initPlatformCapabilities,
  isDesktop,
  platformCapabilities,
} from "./platform";

export const locked = ref(false); // resolved by initLock()
export const unlocking = ref(false);
export const lockError = ref<string | null>(null);
export { isDesktop };
export const supportsBiometrics = computed(
  () => platformCapabilities.value.supportsBiometrics,
);

// Phone-approval state, for the lock screen to reflect.
export const phoneWaiting = ref(false);
export const phoneError = ref<string | null>(null);

const LKEY = "jarvis.lock.enabled";
export const lockEnabled = ref(localStorage.getItem(LKEY) === "true");

// Once unlocked, stay unlocked for the rest of this window session. sessionStorage
// survives hot-reloads and page reloads but clears when the app is closed, so a
// dev code-save never re-prompts for Touch ID, while a fresh launch still locks.
const SESSION_UNLOCKED = "jarvis.unlocked";

// Auto re-lock after this much inactivity (ms). 0 disables it — the app then
// only locks on a fresh launch, so you authenticate once per session, not
// repeatedly. (A configurable timeout can come back later if wanted.)
const INACTIVITY_MS = 0;
let idleTimer: number | undefined;
let pollActive = false;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Decide whether this platform locks in-app (desktop yes, phone no). */
export async function initLock(): Promise<void> {
  await initPlatformCapabilities();
  const alreadyUnlocked = sessionStorage.getItem(SESSION_UNLOCKED) === "1";
  locked.value = isDesktop.value && lockEnabled.value && !alreadyUnlocked;
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
  sessionStorage.setItem(SESSION_UNLOCKED, "1"); // survive reloads this session
  armIdle();
}

export function lockApp(): void {
  if (isDesktop.value && lockEnabled.value) {
    sessionStorage.removeItem(SESSION_UNLOCKED);
    locked.value = true;
  }
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

/** Ask a trusted phone to approve the unlock; long-poll until it resolves. */
export async function requestPhoneApproval(): Promise<void> {
  phoneError.value = null;
  const status = await currentAuthStatus();
  if (!status.authenticated) {
    phoneError.value = "niet ingelogd";
    return;
  }
  try {
    const { request_id } = await postJsonAuth<{ request_id: string; nonce: string }>(
      "/v1/auth/unlock/request",
      {},
    );
    phoneWaiting.value = true;
    pollActive = true;
    const deadline = Date.now() + 2 * 60 * 1000;
    // Long-poll: the server holds each request ~20s and returns the instant the
    // phone acts, so approval feels immediate without a tight polling loop.
    while (pollActive && Date.now() < deadline) {
      try {
        const { status } = await getJsonAuth<{ status: string }>(
          `/v1/auth/unlock/${request_id}?wait=20`,
        );
        if (!pollActive) return; // cancelled while the request was open
        if (status === "approved") {
          unlockApp();
          return;
        }
        if (status === "denied") {
          stopPhonePoll();
          phoneError.value = "geweigerd op je telefoon";
          return;
        }
        if (status === "expired") {
          stopPhonePoll();
          phoneError.value = "verzoek verlopen — probeer opnieuw";
          return;
        }
        // still pending → loop immediately (the server already waited)
      } catch {
        if (!pollActive) return;
        await sleep(1500); // transient network error — back off, then retry
      }
    }
    if (pollActive) {
      stopPhonePoll();
      phoneError.value = "verzoek verlopen — probeer opnieuw";
    }
  } catch (e) {
    stopPhonePoll();
    phoneError.value = e instanceof Error ? e.message : String(e);
  }
}

export function cancelPhoneApproval(): void {
  stopPhonePoll();
  phoneError.value = null;
}

function stopPhonePoll(): void {
  pollActive = false;
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
