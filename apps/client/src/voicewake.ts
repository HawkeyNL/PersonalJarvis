// Voice activation is being re-homed: Picovoice is unavailable (its signup
// rejects personal email domains), so the engine is being chosen — local
// (Vosk, no account) vs server-side (heavier open models + profile sync).
//
// This is an inert stub so the app keeps building and the wiring points
// (Settings, the console's wakePulse, App start/stop) stay intact. The feature
// is off; nothing here touches the mic.
import { ref, computed } from "vue";

export const accessKey = ref("");
export const wakeEnabled = ref(false);
export const enrolled = ref(false);
export const enrolling = ref(false);
export const enrollPct = ref(0);
export const wakeRunning = ref(false);
export const wakeStatus = ref("uit — voice-engine wordt gekozen");
export const wakeError = ref<string | null>(null);
export const lastScore = ref(0);

/** Bumped on a verified wake; the console listens. No-op while re-homing. */
export const wakePulse = ref(0);

export const wakeReady = computed(() => false);

export function setAccessKey(_k: string): void {
  /* no-op while the engine is being chosen */
}
export async function enrollVoice(): Promise<boolean> {
  return false;
}
export async function startWake(): Promise<void> {
  /* no-op */
}
export async function stopWake(): Promise<void> {
  /* no-op */
}
export async function setWakeEnabled(_v: boolean): Promise<void> {
  /* no-op */
}
export async function maybeStartWake(): Promise<void> {
  /* no-op */
}
