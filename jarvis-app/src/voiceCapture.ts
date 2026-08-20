// Record a short utterance as 16 kHz mono 16-bit PCM, ready to upload to the
// server-side speech engine (STT + speaker verification).
//
// Uses WebAudio (works in WKWebView). We capture at the device sample rate via a
// ScriptProcessor and resample to 16 kHz — what the speech models expect.

import { micConstraints } from "./micDevices";

const TARGET_RATE = 16000;

/* eslint-disable @typescript-eslint/no-explicit-any */
const AC: typeof AudioContext =
  window.AudioContext ?? (window as any).webkitAudioContext;
/* eslint-enable @typescript-eslint/no-explicit-any */

export const captureSupported = !!(navigator.mediaDevices && AC);

export interface Pcm {
  sampleRate: number;
  pcm: Int16Array;
}

function floatToInt16(f: Float32Array): Int16Array {
  const out = new Int16Array(f.length);
  for (let i = 0; i < f.length; i++) {
    const s = Math.max(-1, Math.min(1, f[i]));
    out[i] = Math.round(s * 32767);
  }
  return out;
}

function resampleTo16k(input: Float32Array, inRate: number): Int16Array {
  if (inRate === TARGET_RATE) return floatToInt16(input);
  const ratio = inRate / TARGET_RATE;
  const outLen = Math.max(1, Math.floor(input.length / ratio));
  const out = new Int16Array(outLen);
  for (let i = 0; i < outLen; i++) {
    const idx = i * ratio;
    const i0 = Math.floor(idx);
    const frac = idx - i0;
    const a = input[i0] ?? 0;
    const b = input[i0 + 1] ?? a;
    const s = a * (1 - frac) + b * frac;
    out[i] = Math.max(-32768, Math.min(32767, Math.round(s * 32767)));
  }
  return out;
}

/** Record for `ms` milliseconds and return 16 kHz mono PCM. */
export async function recordPcm(ms: number): Promise<Pcm> {
  if (!captureSupported) throw new Error("microfoon niet beschikbaar");
  const stream = await navigator.mediaDevices.getUserMedia(micConstraints());
  const ctx = new AC();
  const source = ctx.createMediaStreamSource(stream);
  const bufferSize = 4096;
  const processor = ctx.createScriptProcessor(bufferSize, 1, 1);
  const mute = ctx.createGain();
  mute.gain.value = 0; // route to destination (needed to run) without echo

  const chunks: Float32Array[] = [];
  processor.onaudioprocess = (e: AudioProcessingEvent) => {
    chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
  };

  source.connect(processor);
  processor.connect(mute);
  mute.connect(ctx.destination);

  try {
    await new Promise((r) => setTimeout(r, ms));
  } finally {
    processor.disconnect();
    source.disconnect();
    mute.disconnect();
    stream.getTracks().forEach((t) => t.stop());
  }

  const inRate = ctx.sampleRate;
  await ctx.close();

  const total = chunks.reduce((n, c) => n + c.length, 0);
  const merged = new Float32Array(total);
  let off = 0;
  for (const c of chunks) {
    merged.set(c, off);
    off += c.length;
  }
  return { sampleRate: TARGET_RATE, pcm: resampleTo16k(merged, inRate) };
}
