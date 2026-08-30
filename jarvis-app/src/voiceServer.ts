// Server-side speech: enroll a voice profile and verify a live utterance
// against it. The heavy lifting (STT + speaker embedding) runs on our own
// backend behind the SpeechEngine trait; the profile is stored centrally so it
// is shared across all this user's devices (see ADR-025).
//
// Security posture: voice is *convenience*, never the lock. A positive verify
// may open the console or kick off biometrics — it never replaces them.

import { ref } from "vue";
import { currentAuthStatus } from "./auth";
import { getJsonAuth, postJsonAuth } from "./api";
import { recordPcm, captureSupported } from "./voiceCapture";

export const voiceSupported = captureSupported;

/** Whether a voice profile exists on the server for this user. */
export const enrolled = ref(false);
/** Engine label reported by the server (e.g. "stub", "whisper:base"). */
export const engine = ref("");
/** True while a record/upload round-trip is in flight. */
export const busy = ref(false);
/** Human-readable status line for the UI. */
export const voiceStatus = ref("");
/** Last error, or null. */
export const voiceError = ref<string | null>(null);

export interface VerifyResult {
  enrolled: boolean;
  is_you: boolean;
  score: number;
  transcript: string;
}
/** Result of the most recent verify(), or null. */
export const lastVerify = ref<VerifyResult | null>(null);

async function requireAuth(): Promise<void> {
  const status = await currentAuthStatus();
  if (!status.authenticated) throw new Error("niet ingelogd");
}

/** i16 PCM → plain number[] so JSON.stringify emits a real array. */
function pcmArray(pcm: Int16Array): number[] {
  return Array.from(pcm);
}

/** Refresh the enrolled/engine state from the server. Silent on failure. */
export async function refreshVoiceStatus(): Promise<void> {
  try {
    await requireAuth();
    const res = await getJsonAuth<{ enrolled: boolean; engine: string }>(
      "/v1/voice/status",
    );
    enrolled.value = res.enrolled;
    engine.value = res.engine;
  } catch {
    /* not logged in yet, or backend down — leave state as-is */
  }
}

/** Record `seconds` of speech and store it as the user's voice profile. */
export async function enroll(seconds = 4): Promise<void> {
  if (busy.value) return;
  if (!voiceSupported) {
    voiceError.value = "microfoon niet beschikbaar";
    return;
  }
  busy.value = true;
  voiceError.value = null;
  lastVerify.value = null;
  voiceStatus.value = `opnemen… praat rustig (${seconds}s)`;
  try {
    const rec = await recordPcm(seconds * 1000);
    voiceStatus.value = "versturen…";
    await requireAuth();
    const res = await postJsonAuth<{ status: string; dims: number }>(
      "/v1/voice/enroll",
      { sample_rate: rec.sampleRate, pcm: pcmArray(rec.pcm) },
    );
    voiceStatus.value = `stemprofiel opgeslagen ✓ (${res.dims}-dim)`;
    await refreshVoiceStatus();
  } catch (e) {
    voiceError.value = e instanceof Error ? e.message : String(e);
    voiceStatus.value = "";
  } finally {
    busy.value = false;
  }
}

/**
 * Record a short utterance, send it to the server, and return whether it
 * matches the enrolled speaker (plus transcript + score). Also stored in
 * `lastVerify` for the UI.
 */
export async function verify(seconds = 3): Promise<VerifyResult | null> {
  if (busy.value) return null;
  if (!voiceSupported) {
    voiceError.value = "microfoon niet beschikbaar";
    return null;
  }
  busy.value = true;
  voiceError.value = null;
  lastVerify.value = null;
  voiceStatus.value = `opnemen… (${seconds}s)`;
  try {
    const rec = await recordPcm(seconds * 1000);
    voiceStatus.value = "controleren…";
    await requireAuth();
    const res = await postJsonAuth<VerifyResult>("/v1/voice/verify", {
      sample_rate: rec.sampleRate,
      pcm: pcmArray(rec.pcm),
    });
    lastVerify.value = res;
    voiceStatus.value = !res.enrolled
      ? "geen profiel — schrijf je eerst in"
      : res.is_you
        ? `herkend ✓ (${res.score.toFixed(2)})`
        : `niet herkend (${res.score.toFixed(2)})`;
    return res;
  } catch (e) {
    voiceError.value = e instanceof Error ? e.message : String(e);
    voiceStatus.value = "";
    return null;
  } finally {
    busy.value = false;
  }
}
