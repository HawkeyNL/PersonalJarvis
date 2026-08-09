// "Hey Jarvis" wake-word controller.
//
// Cross-device by construction: this lives in the shared Tauri webview, so one
// implementation covers macOS and iOS (see ADR-026). The always-on detector
// (openWakeWord "hey_jarvis" via onnxruntime-web) plugs in behind `triggerWake`
// — every detection, manual or automatic, runs the same path.
//
// Security posture: wake/voice is *convenience*, never the lock. A wake reveals
// the console (and, when a profile is enrolled, only for *your* voice); if the
// app is locked, downstream flows still require Touch ID / phone approval.
import { ref, computed } from "vue";
import { enrolled, verify, refreshVoiceStatus, voiceSupported } from "./voiceServer";

const WAKE_KEY = "jarvis.wake.enabled";

export const wakeEnabled = ref(localStorage.getItem(WAKE_KEY) === "1");
export const wakeRunning = ref(false);
export const wakeStatus = ref("uit");
export const wakeError = ref<string | null>(null);
export const lastScore = ref(0);

/** Bumped on a verified wake; the console watches this to reveal + listen. */
export const wakePulse = ref(0);

/** The feature is usable on any device; the speaker-gate needs a mic. */
export const wakeReady = computed(() => true);

let verifying = false;
let keyHandler: ((e: KeyboardEvent) => void) | null = null;
let unlistenNative: (() => void) | null = null;

// Temporary manual trigger until the always-on detector lands: ⌘/Ctrl+⇧+J in the
// webview simulates "Hey Jarvis" and runs the exact same path a real detection
// will.
function matchesHotkey(e: KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "J" || e.key === "j");
}

/**
 * Run the wake path: speaker-gate against the server (when enrolled), then
 * reveal the console. Only *your* voice wakes Jarvis once a profile exists.
 */
export async function triggerWake(): Promise<void> {
  if (verifying) return;
  verifying = true;
  wakeError.value = null;
  try {
    if (enrolled.value && voiceSupported) {
      wakeStatus.value = "stem controleren…";
      const res = await verify(3);
      lastScore.value = res?.score ?? 0;
      if (res && res.enrolled && !res.is_you) {
        wakeStatus.value = `niet herkend (${res.score.toFixed(2)})`;
        return; // not your voice → do not wake
      }
    }
    wakeStatus.value = "Hey Jarvis 👋";
    wakePulse.value++;
  } catch (e) {
    wakeError.value = e instanceof Error ? e.message : String(e);
  } finally {
    verifying = false;
  }
}

export async function startWake(): Promise<void> {
  if (wakeRunning.value) return;

  keyHandler = (e) => {
    if (matchesHotkey(e)) {
      e.preventDefault();
      void triggerWake();
    }
  };
  window.addEventListener("keydown", keyHandler);

  // Native detector hook: a future openWakeWord process (native or WASM worker)
  // can emit "wake-detected"; it flows through the same gate.
  try {
    const { listen } = await import("@tauri-apps/api/event");
    unlistenNative = await listen("wake-detected", () => void triggerWake());
  } catch {
    /* not under Tauri (plain browser) — the hotkey path still works */
  }

  await refreshVoiceStatus();
  wakeRunning.value = true;
  wakeStatus.value = "gereed — zeg ‘Hey Jarvis’ (⌘⇧J tot auto-detectie)";
}

export async function stopWake(): Promise<void> {
  if (keyHandler) {
    window.removeEventListener("keydown", keyHandler);
    keyHandler = null;
  }
  if (unlistenNative) {
    unlistenNative();
    unlistenNative = null;
  }
  wakeRunning.value = false;
  wakeStatus.value = "uit";
}

export async function setWakeEnabled(v: boolean): Promise<void> {
  wakeEnabled.value = v;
  localStorage.setItem(WAKE_KEY, v ? "1" : "0");
  if (v) await startWake();
  else await stopWake();
}

/** Resume the wake listener on app start if it was enabled. */
export async function maybeStartWake(): Promise<void> {
  if (wakeEnabled.value) await startWake();
}
