// Voice output policy + TTS. Decides WHEN Jarvis may speak aloud.
//
// The audio route (earbud vs built-in speaker) decides speech, not the input
// modality — so Jarvis speaks through your earbud even when you type back.
// See voice/CONVERSATION_AND_OUTPUT_POLICY.md and decisions/ADR-021.
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

export type AudioRoute = "headset" | "speaker" | "unknown";

const VKEY = "jarvis.voice.enabled";
const SKEY = "jarvis.voice.allowSpeaker";
const HKEY = "jarvis.voice.headset";

// Master voice output (default on).
export const voiceEnabled = ref(localStorage.getItem(VKEY) !== "false");
// Allow speaking on the open speaker route (default off — stay quiet in the open).
export const allowSpeaker = ref(localStorage.getItem(SKEY) === "true");
// Manual "earbud in" override until native route detection lands.
export const headset = ref(localStorage.getItem(HKEY) === "true");
// Detected route.
export const route = ref<AudioRoute>("unknown");

export function setVoiceEnabled(v: boolean) {
  voiceEnabled.value = v;
  localStorage.setItem(VKEY, String(v));
  if (!v) stopSpeaking();
}
export function setAllowSpeaker(v: boolean) {
  allowSpeaker.value = v;
  localStorage.setItem(SKEY, String(v));
}
export function setHeadset(v: boolean) {
  headset.value = v;
  localStorage.setItem(HKEY, String(v));
}

/** Ask the native layer for the route; fall back to the manual toggle. */
export async function refreshRoute(): Promise<AudioRoute> {
  try {
    const r = await invoke<string>("audio_output_route");
    if (r === "headset" || r === "speaker") {
      route.value = r;
      return route.value;
    }
  } catch {
    /* command not available yet — use the manual override */
  }
  route.value = headset.value ? "headset" : "unknown";
  return route.value;
}

function isPrivateRoute(): boolean {
  if (route.value === "headset") return true;
  if (route.value === "speaker") return false;
  return headset.value; // unknown → honour manual toggle
}

/** The policy: may Jarvis speak right now, and why? */
export function canSpeak(): { allowed: boolean; reason: string } {
  if (!voiceEnabled.value) return { allowed: false, reason: "spraak uit" };
  if (isPrivateRoute()) return { allowed: true, reason: "oortje verbonden" };
  if (allowSpeaker.value) return { allowed: true, reason: "luidspreker toegestaan" };
  return { allowed: false, reason: "geen oortje — stil" };
}

/** Speak text if the policy allows. Returns whether it spoke. */
export function speak(text: string): boolean {
  if (!canSpeak().allowed) return false;
  if (typeof window === "undefined" || !("speechSynthesis" in window)) return false;
  stopSpeaking();
  const u = new SpeechSynthesisUtterance(text);
  u.lang = "nl-NL";
  window.speechSynthesis.speak(u);
  return true;
}

export function stopSpeaking(): void {
  if (typeof window !== "undefined" && "speechSynthesis" in window) {
    window.speechSynthesis.cancel();
  }
}
