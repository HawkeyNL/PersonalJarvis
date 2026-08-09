// "Hey Jarvis" wake word + speaker verification (only your voice), on-device.
//
// Porcupine spots the built-in "Jarvis" keyword; Eagle scores whether the voice
// matches your enrolled profile. Both run locally (WASM) via Picovoice — no
// audio leaves the device. Everything here is opt-in and dynamically imported,
// so when it's off the app is completely unaffected.
//
// Security posture (per ADR): voice is a *convenience*, not the security gate.
// When the app is locked it only *starts* the biometric prompt; it never bypasses
// Touch ID / phone approval. When unlocked it just wakes the console.
import { ref, computed } from "vue";
import { locked, biometricUnlock } from "./lock";

const KEY_ACCESS = "jarvis.pv.accessKey";
const KEY_PROFILE = "jarvis.pv.profile";
const KEY_WAKE = "jarvis.wake.enabled";

const PORCUPINE_MODEL = "/models/porcupine_params.pv";
const EAGLE_MODEL = "/models/eagle_params.pv";
// Eagle scores are [0,1]; a match above this is treated as "you".
const WAKE_THRESHOLD = 0.5;

export const accessKey = ref(localStorage.getItem(KEY_ACCESS) ?? "");
export const wakeEnabled = ref(localStorage.getItem(KEY_WAKE) === "true");
export const enrolled = ref(!!localStorage.getItem(KEY_PROFILE));
export const enrolling = ref(false);
export const enrollPct = ref(0);
export const wakeRunning = ref(false);
export const wakeStatus = ref("uit");
export const wakeError = ref<string | null>(null);
export const lastScore = ref(0);

/** Bumped on a verified "Hey Jarvis" while unlocked — the console listens. */
export const wakePulse = ref(0);

export const wakeReady = computed(() => !!accessKey.value.trim() && enrolled.value);

// Picovoice handles (dynamically imported; typed loosely on purpose).
/* eslint-disable @typescript-eslint/no-explicit-any */
let porcupine: any = null;
let eagle: any = null;
let feeder: any = null;
/* eslint-enable @typescript-eslint/no-explicit-any */

function toB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}
function fromB64(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}
function loadProfile(): Uint8Array | null {
  const b64 = localStorage.getItem(KEY_PROFILE);
  return b64 ? fromB64(b64) : null;
}
function saveProfile(bytes: Uint8Array): void {
  localStorage.setItem(KEY_PROFILE, toB64(bytes));
  enrolled.value = true;
}

export function setAccessKey(k: string): void {
  accessKey.value = k;
  localStorage.setItem(KEY_ACCESS, k);
}

export function clearEnrollment(): void {
  localStorage.removeItem(KEY_PROFILE);
  enrolled.value = false;
}

/** What happens on a verified wake. Convenience only — never bypasses the lock. */
function onWakeVerified(): void {
  if (locked.value) {
    biometricUnlock(); // start Touch ID; biometrics still required
  } else {
    wakePulse.value += 1; // the console reveals + starts listening
  }
}

function appendFrame(buf: Int16Array, frame: Int16Array): Int16Array {
  const merged = new Int16Array(buf.length + frame.length);
  merged.set(buf);
  merged.set(frame, buf.length);
  return merged;
}

/** Record and enroll your voice until the profile is complete. */
export async function enrollVoice(): Promise<boolean> {
  if (enrolling.value) return false;
  const key = accessKey.value.trim();
  if (!key) {
    wakeError.value = "AccessKey ontbreekt";
    return false;
  }
  enrolling.value = true;
  enrollPct.value = 0;
  wakeError.value = null;
  const resume = wakeRunning.value;
  if (resume) await stopWake();

  /* eslint-disable @typescript-eslint/no-explicit-any */
  let profiler: any = null;
  let enrollFeeder: any = null;
  /* eslint-enable @typescript-eslint/no-explicit-any */
  const { WebVoiceProcessor } = await import("@picovoice/web-voice-processor");
  try {
    const { EagleProfilerWorker } = await import("@picovoice/eagle-web");
    profiler = await EagleProfilerWorker.create(key, { publicPath: EAGLE_MODEL });
    const need: number = profiler.frameLength;
    let buf = new Int16Array(0);
    let busy = false;
    let done = false;

    await new Promise<void>((resolve, reject) => {
      const timeout = window.setTimeout(() => {
        if (!done) {
          done = true;
          reject(new Error("time-out — praat in een stille ruimte en probeer opnieuw"));
        }
      }, 60000);
      enrollFeeder = {
        postMessage: async (e: { command: string; inputFrame?: Int16Array }) => {
          if (done || e.command !== "process" || !e.inputFrame) return;
          buf = appendFrame(buf, e.inputFrame);
          if (buf.length < need || busy) return;
          busy = true;
          const chunk = buf.slice(0, need);
          buf = buf.slice(need);
          try {
            const pct: number = await profiler.enroll(chunk);
            enrollPct.value = Math.round(pct);
            if (pct >= 100 && !done) {
              done = true;
              window.clearTimeout(timeout);
              resolve();
            }
          } catch (err) {
            if (!done) {
              done = true;
              window.clearTimeout(timeout);
              reject(err);
            }
          } finally {
            busy = false;
          }
        },
      };
      WebVoiceProcessor.subscribe(enrollFeeder);
    });

    const profile = await profiler.export();
    saveProfile(profile.bytes);
    wakeStatus.value = "stem opgeslagen ✓";
    return true;
  } catch (e) {
    wakeError.value = e instanceof Error ? e.message : String(e);
    return false;
  } finally {
    try {
      if (enrollFeeder) await WebVoiceProcessor.unsubscribe(enrollFeeder);
    } catch {
      /* ignore */
    }
    try {
      await profiler?.terminate?.();
    } catch {
      /* ignore */
    }
    enrolling.value = false;
    if (resume) startWake();
  }
}

/** Start listening for "Hey Jarvis" (prompts for mic permission). */
export async function startWake(): Promise<void> {
  if (wakeRunning.value || enrolling.value) return;
  const key = accessKey.value.trim();
  const profileBytes = loadProfile();
  if (!key) {
    wakeStatus.value = "AccessKey ontbreekt";
    return;
  }
  if (!profileBytes) {
    wakeStatus.value = "neem eerst je stem op";
    return;
  }
  wakeError.value = null;
  try {
    const { PorcupineWorker, BuiltInKeyword } = await import("@picovoice/porcupine-web");
    const { EagleWorker } = await import("@picovoice/eagle-web");
    const { WebVoiceProcessor } = await import("@picovoice/web-voice-processor");

    const profile = { bytes: profileBytes };
    eagle = await EagleWorker.create(key, { publicPath: EAGLE_MODEL });
    porcupine = await PorcupineWorker.create(
      key,
      BuiltInKeyword.Jarvis,
      () => onWake(),
      { publicPath: PORCUPINE_MODEL },
    );

    const need: number = eagle.minProcessSamples;
    let buf = new Int16Array(0);
    let busy = false;
    feeder = {
      postMessage: (e: { command: string; inputFrame?: Int16Array }) => {
        if (e.command !== "process" || !e.inputFrame) return;
        buf = appendFrame(buf, e.inputFrame);
        if (buf.length < need || busy) return;
        busy = true;
        const chunk = buf.slice(0, need);
        buf = buf.slice(need);
        eagle
          .process(chunk, profile)
          .then((s: number[] | null) => {
            if (s && s.length) lastScore.value = s[0];
          })
          .catch(() => {})
          .finally(() => {
            busy = false;
          });
      },
    };

    await WebVoiceProcessor.subscribe([porcupine, feeder]);
    wakeRunning.value = true;
    wakeStatus.value = "luistert naar “Hey Jarvis”";
  } catch (e) {
    wakeError.value = e instanceof Error ? e.message : String(e);
    await stopWake();
  }
}

export async function stopWake(): Promise<void> {
  try {
    const { WebVoiceProcessor } = await import("@picovoice/web-voice-processor");
    const subs = [porcupine, feeder].filter(Boolean);
    if (subs.length) await WebVoiceProcessor.unsubscribe(subs);
  } catch {
    /* ignore */
  }
  try {
    await porcupine?.terminate?.();
  } catch {
    /* ignore */
  }
  try {
    await eagle?.terminate?.();
  } catch {
    /* ignore */
  }
  porcupine = null;
  eagle = null;
  feeder = null;
  wakeRunning.value = false;
  if (wakeStatus.value.startsWith("luistert")) wakeStatus.value = "uit";
}

function onWake(): void {
  // Only act if the voice matches your enrolled profile.
  if (lastScore.value >= WAKE_THRESHOLD) {
    wakeStatus.value = "herkend ✓";
    onWakeVerified();
  } else {
    wakeStatus.value = "stem niet herkend";
  }
}

export async function setWakeEnabled(v: boolean): Promise<void> {
  wakeEnabled.value = v;
  localStorage.setItem(KEY_WAKE, String(v));
  if (v) await startWake();
  else await stopWake();
}

/** Called on app start: resume listening if the user had it on and is ready. */
export async function maybeStartWake(): Promise<void> {
  if (wakeEnabled.value && wakeReady.value) await startWake();
}
