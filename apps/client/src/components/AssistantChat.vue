<script setup lang="ts">
import { ref, computed, watch, nextTick, onMounted, onBeforeUnmount } from "vue";
import NavIcon from "./NavIcon.vue";
import { messages, send } from "../assistant";
import {
  voiceEnabled,
  headset,
  setVoiceEnabled,
  setHeadset,
  canSpeak,
  refreshRoute,
} from "../voice";

const text = ref("");
const listEl = ref<HTMLElement | null>(null);
const policy = computed(() => canSpeak());

const listening = ref(false);
const level = ref(0); // live mic loudness 0..1 → drives the uplight

const AC =
  window.AudioContext ??
  (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
const micAvailable = !!(navigator.mediaDevices && AC);

// Best-effort transcript (webview STT; often absent in WKWebView — real STT = DEC-009).
const SR =
  (window as unknown as { webkitSpeechRecognition?: new () => unknown }).webkitSpeechRecognition ??
  (window as unknown as { SpeechRecognition?: new () => unknown }).SpeechRecognition;

const SILENCE_MS = 5000;
const SPEAK_LEVEL = 0.06;

let stream: MediaStream | null = null;
let ctx: AudioContext | null = null;
let analyser: AnalyserNode | null = null;
let data: Uint8Array | null = null;
let raf = 0;
let silence: number | undefined;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let recog: any = null;

function onSend() {
  if (!text.value.trim()) return;
  send(text.value);
  text.value = "";
}

function armSilence() {
  clearTimeout(silence);
  silence = window.setTimeout(stopMic, SILENCE_MS); // auto-off after 5s of quiet
}

function loop() {
  if (!analyser || !data) return;
  analyser.getByteTimeDomainData(data);
  let sum = 0;
  for (let i = 0; i < data.length; i++) {
    const x = (data[i] - 128) / 128;
    sum += x * x;
  }
  const rms = Math.sqrt(sum / data.length);
  level.value = Math.max(level.value * 0.7, Math.min(1, rms * 4)); // smooth + scale
  if (level.value > SPEAK_LEVEL) armSilence(); // reset the timer while you talk
  raf = requestAnimationFrame(loop);
}

async function startMic() {
  const md = navigator.mediaDevices;
  if (!md || !AC) return;
  try {
    stream = await md.getUserMedia({ audio: true });
  } catch {
    return; // permission denied / no mic
  }
  ctx = new AC();
  const src = ctx.createMediaStreamSource(stream);
  analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  src.connect(analyser);
  data = new Uint8Array(analyser.frequencyBinCount);
  listening.value = true;
  armSilence();
  loop();
  startRecognition();
}

function startRecognition() {
  if (!SR) return;
  try {
    recog = new SR();
    recog.lang = "nl-NL";
    recog.interimResults = false;
    recog.continuous = true;
    recog.onresult = (e: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => {
      const last = e.results[e.results.length - 1];
      const said = last && last[0] ? last[0].transcript : "";
      if (said.trim()) send(said);
      armSilence();
    };
    recog.onend = () => {
      recog = null;
    };
    recog.start();
  } catch {
    recog = null;
  }
}

function stopMic() {
  clearTimeout(silence);
  cancelAnimationFrame(raf);
  if (recog) {
    try {
      recog.stop();
    } catch {
      /* ignore */
    }
    recog = null;
  }
  if (stream) {
    stream.getTracks().forEach((t) => t.stop());
    stream = null;
  }
  if (ctx) {
    ctx.close().catch(() => {});
    ctx = null;
  }
  analyser = null;
  data = null;
  level.value = 0;
  listening.value = false;
}

function toggleMic() {
  if (listening.value) stopMic();
  else startMic();
}

// Uplight: glow intensity follows your voice while listening.
const micStyle = computed(() =>
  listening.value
    ? {
        borderColor: "var(--accent)",
        boxShadow: `0 0 ${(8 + level.value * 26).toFixed(0)}px rgba(52,245,160,${(0.3 + level.value * 0.55).toFixed(2)})`,
      }
    : {},
);

function toggleVoice() {
  setVoiceEnabled(!voiceEnabled.value);
}
async function toggleHeadset() {
  setHeadset(!headset.value);
  await refreshRoute();
}

watch(
  () => messages.value.length,
  async () => {
    await nextTick();
    if (listEl.value) listEl.value.scrollTop = listEl.value.scrollHeight;
  },
);

onMounted(refreshRoute);
onBeforeUnmount(stopMic);
</script>

<template>
  <div class="chat">
    <div class="chat-head">
      <span class="ch-title">GESPREK</span>
      <div class="ch-tools">
        <button
          class="ic"
          :class="{ on: voiceEnabled }"
          @click="toggleVoice"
          :title="voiceEnabled ? 'Spraak uit' : 'Spraak aan'"
        >
          <NavIcon :name="voiceEnabled ? 'sound-on' : 'sound-off'" />
        </button>
        <button
          class="ic"
          :class="{ on: headset }"
          @click="toggleHeadset"
          title="Oortje in/uit"
        >
          <NavIcon name="headset" />
        </button>
      </div>
    </div>

    <div class="ch-policy" :class="policy.allowed ? 'ok' : 'off'">
      <span class="dot" :class="policy.allowed ? 'dot-ok' : 'dot-todo'"></span>
      {{ policy.allowed ? "Jarvis kan praten" : "Jarvis is stil" }} · {{ policy.reason }}
    </div>

    <div ref="listEl" class="ch-list">
      <div v-for="m in messages" :key="m.id" class="msg" :class="m.role">
        <div class="bubble">{{ m.text }}</div>
        <div class="meta">
          <span v-if="m.role === 'jarvis' && m.spoken" class="spoke">🔊</span>{{ m.ts }}
        </div>
      </div>
      <p v-if="!messages.length" class="ch-empty">Zeg of typ iets tegen Jarvis…</p>
    </div>

    <form class="ch-input" @submit.prevent="onSend">
      <button
        type="button"
        class="mic"
        :class="{ live: listening }"
        :style="micStyle"
        :disabled="!micAvailable"
        :title="micAvailable ? (listening ? 'Luistert… (stopt na 5s stilte)' : 'Spreken') : 'Spraakinvoer niet beschikbaar'"
        @click="toggleMic"
      >
        <NavIcon name="mic" />
      </button>
      <input v-model="text" placeholder="Typ tegen Jarvis…" aria-label="bericht" />
      <button type="submit" class="send" aria-label="verstuur"><NavIcon name="send" /></button>
    </form>
  </div>
</template>

<style scoped>
.chat {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 320px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: linear-gradient(180deg, rgba(16, 34, 26, 0.5), rgba(8, 18, 14, 0.42));
  backdrop-filter: blur(12px) saturate(1.25);
  -webkit-backdrop-filter: blur(12px) saturate(1.25);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
  overflow: hidden;
}

.chat-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 11px 13px;
  border-bottom: 1px solid var(--border);
}
.ch-title { font-family: var(--mono); font-size: 11px; letter-spacing: 0.18em; color: var(--accent); }
.ch-tools { display: flex; gap: 6px; }
.ic {
  background: transparent; border: 1px solid var(--border); border-radius: 8px;
  padding: 5px; cursor: pointer; color: var(--muted);
  display: inline-flex; align-items: center; justify-content: center;
}
.ic svg { width: 16px; height: 16px; display: block; }
.ic:hover { border-color: var(--accent); color: var(--text); filter: none; }
.ic.on { color: var(--accent); border-color: var(--accent); box-shadow: 0 0 8px rgba(52, 245, 160, 0.35); }

.ch-policy {
  display: flex; align-items: center; gap: 8px;
  padding: 8px 13px; font-family: var(--mono); font-size: 10.5px; letter-spacing: 0.04em;
  color: var(--muted); border-bottom: 1px solid var(--border);
}
.ch-policy.ok { color: var(--accent); }

.ch-list {
  flex: 1; overflow-y: auto; padding: 12px 13px;
  display: flex; flex-direction: column; gap: 10px;
}
.ch-empty { color: var(--muted); font-size: 13px; margin: auto 0; text-align: center; }

.msg { display: flex; flex-direction: column; gap: 3px; max-width: 88%; }
.msg.user { align-self: flex-end; align-items: flex-end; }
.msg.jarvis { align-self: flex-start; align-items: flex-start; }
.bubble {
  padding: 8px 12px; border-radius: 14px; font-size: 13.5px; line-height: 1.4;
}
.msg.user .bubble {
  background: linear-gradient(180deg, var(--accent), var(--accent-2));
  color: #04140c; border-bottom-right-radius: 4px; font-weight: 500;
}
.msg.jarvis .bubble {
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid var(--border); color: var(--text); border-bottom-left-radius: 4px;
}
.meta { font-family: var(--mono); font-size: 9px; color: var(--muted); letter-spacing: 0.06em; }
.spoke { margin-right: 4px; }

.ch-input {
  display: flex; align-items: center; gap: 7px; padding: 10px 11px;
  border-top: 1px solid var(--border);
}
.ch-input input {
  flex: 1; min-width: 0;
  background: rgba(0, 0, 0, 0.25); border: 1px solid var(--border); color: var(--text);
  border-radius: 10px; padding: 9px 12px; font: inherit; font-size: 13px;
}
.ch-input input:focus { outline: none; border-color: var(--accent); }
.mic, .send {
  flex: none; width: 38px; height: 38px; border-radius: 10px; cursor: pointer;
  display: inline-flex; align-items: center; justify-content: center; font-size: 15px;
}
.mic { background: transparent; border: 1px solid var(--border); color: var(--text); }
.mic:hover:not(:disabled) { border-color: var(--accent); filter: none; }
.mic:disabled { opacity: 0.4; cursor: not-allowed; }
.mic.live { border-color: var(--accent); color: var(--accent); } /* glow set inline, scales with voice */
.send { background: var(--accent); color: #04140c; border: none; font-weight: 700; }
.mic svg { width: 18px; height: 18px; }
.send svg { width: 17px; height: 17px; }

@keyframes pulse-mic { 0%, 100% { box-shadow: 0 0 8px rgba(52,245,160,0.4); } 50% { box-shadow: 0 0 16px rgba(52,245,160,0.7); } }
@media (prefers-reduced-motion: reduce) { .mic.live { animation: none; } }
</style>
