<script setup lang="ts">
import { ref, computed, watch, onMounted } from "vue";
import NavIcon from "./NavIcon.vue";
import {
  messages,
  send,
  thinking,
  openConversation,
  startNewConversation,
  initChat,
} from "../assistant";
import { conversations, currentId, deleteConversation } from "../conversations";
import { renderMarkdown, handleMarkdownClick } from "../markdown";
import { useMic } from "../useMic";
import { wakePulse } from "../voicewake";
import {
  voiceEnabled,
  headset,
  setVoiceEnabled,
  setHeadset,
  canSpeak,
  refreshRoute,
} from "../voice";

const text = ref("");
const hovered = ref(false);
const focused = ref(false);
const woke = ref(false);
const policy = computed(() => canSpeak());

const mic = useMic((said) => send(said));

// The input dock lifts into view on hover / focus / typing / while listening,
// and after a "Hey Jarvis" wake; it tucks away again when you leave.
const revealed = computed(
  () => hovered.value || focused.value || woke.value || !!text.value || mic.listening.value,
);

// A verified "Hey Jarvis" reveals the console and starts listening.
let wokeTimer: number | undefined;
watch(wakePulse, () => {
  woke.value = true;
  clearTimeout(wokeTimer);
  wokeTimer = window.setTimeout(() => {
    woke.value = false;
  }, 8000);
  if (!mic.listening.value) mic.toggle();
});

// Only the last few turns float over the backdrop; older ones fade out.
const recent = computed(() => messages.value.slice(-5));

function onSend() {
  if (!text.value.trim()) return;
  send(text.value);
  text.value = "";
}

// Uplight the mic with your voice while listening.
const micStyle = computed(() =>
  mic.listening.value
    ? {
        borderColor: "var(--accent)",
        boxShadow: `0 0 ${(8 + mic.level.value * 26).toFixed(0)}px rgba(52,245,160,${(0.3 + mic.level.value * 0.55).toFixed(2)})`,
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

// Conversation tabs (ADR-030).
function switchTo(id: string) {
  if (id !== currentId.value) void openConversation(id);
}
function newChat() {
  startNewConversation();
}
async function removeChat(id: string) {
  const next = await deleteConversation(id);
  if (next) await openConversation(next);
  else startNewConversation();
}

onMounted(async () => {
  refreshRoute();
  await initChat();
});
</script>

<template>
  <div class="console">
    <div class="stack">
      <!-- Conversation tabs: Jarvis groups chats by topic (ADR-030). -->
      <div class="tabs" v-if="conversations.length">
        <button
          v-for="c in conversations"
          :key="c.id"
          class="tab"
          :class="{ active: c.id === currentId }"
          :title="c.title"
          @click="switchTo(c.id)"
        >
          <span class="tab-title">{{ c.title }}</span>
          <span
            v-if="c.id === currentId"
            class="tab-x"
            title="Gesprek verwijderen"
            @click.stop="removeChat(c.id)"
            >×</span
          >
        </button>
        <button class="tab new" title="Nieuw gesprek" @click="newChat">+</button>
      </div>

      <!-- Conversation, floating over the living backdrop. -->
      <div class="transcript" :class="{ dim: revealed }">
        <TransitionGroup name="line">
        <div v-for="m in recent" :key="m.id" class="line" :class="m.role">
          <span class="who">{{ m.role === "jarvis" ? "JARVIS" : "JIJ" }}</span>
          <!-- Jarvis speaks Markdown; the user's own text stays literal. -->
          <span
            v-if="m.role === 'jarvis'"
            class="txt md"
            @click="handleMarkdownClick"
            v-html="renderMarkdown(m.text)"
          ></span>
          <span v-else class="txt">{{ m.text }}</span>
          <span v-if="m.role === 'jarvis' && m.spoken" class="spoke">🔊</span>
        </div>
      </TransitionGroup>
        <p v-if="thinking" class="line jarvis thinking" key="thinking">
          <span class="who">JARVIS</span>
          <span class="dots"><span></span><span></span><span></span></span>
        </p>
      </div>
    </div>

    <!-- Bottom-left hover zone reveals the input. -->
    <div
      class="zone"
      @mouseenter="hovered = true"
      @mouseleave="hovered = false"
    >
      <div class="peek" :class="{ hide: revealed }">
        <NavIcon name="core" /> praat met Jarvis
      </div>

      <div class="dock" :class="{ show: revealed }">
        <div class="policy" :class="policy.allowed ? 'ok' : 'off'">
          <span class="pdot" :class="policy.allowed ? 'on' : ''"></span>
          {{ policy.allowed ? "Jarvis kan praten" : "Jarvis is stil" }} · {{ policy.reason }}
        </div>
        <form class="row" @submit.prevent="onSend">
          <button
            type="button"
            class="ic mic"
            :class="{ live: mic.listening.value }"
            :style="micStyle"
            :disabled="!mic.available"
            :title="mic.available ? (mic.listening.value ? 'Luistert… (stopt na 5s stilte)' : 'Spreken') : 'Spraakinvoer niet beschikbaar'"
            @click="mic.toggle"
          >
            <NavIcon name="mic" />
          </button>
          <input
            v-model="text"
            placeholder="Typ of spreek tegen Jarvis…"
            aria-label="bericht"
            @focus="focused = true"
            @blur="focused = false"
          />
          <button
            type="button"
            class="ic"
            :class="{ on: voiceEnabled }"
            :title="voiceEnabled ? 'Spraak uit' : 'Spraak aan'"
            @click="toggleVoice"
          >
            <NavIcon :name="voiceEnabled ? 'sound-on' : 'sound-off'" />
          </button>
          <button
            type="button"
            class="ic"
            :class="{ on: headset }"
            title="Oortje in/uit"
            @click="toggleHeadset"
          >
            <NavIcon name="headset" />
          </button>
          <button type="submit" class="ic send" aria-label="verstuur"><NavIcon name="send" /></button>
        </form>
      </div>
    </div>
  </div>
</template>

<style scoped>
.console {
  position: absolute;
  inset: 0;
  z-index: 2;
  pointer-events: none; /* let the backdrop breathe; children re-enable */
}

/* Tabs + transcript stack, floating bottom-left above the input zone. */
.stack {
  position: absolute;
  left: clamp(20px, 5vw, 64px);
  bottom: 132px;
  width: min(46ch, 60vw);
  display: flex;
  flex-direction: column;
  gap: 8px;
  pointer-events: none; /* only the tabs re-enable clicks */
}

/* Conversation tabs — Jarvis groups chats by topic. */
.tabs {
  display: flex;
  gap: 6px;
  overflow-x: auto;
  pointer-events: auto;
  padding-bottom: 2px;
  scrollbar-width: none;
}
.tabs::-webkit-scrollbar {
  display: none;
}
.tab {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 22ch;
  padding: 4px 10px;
  border-radius: 999px;
  cursor: pointer;
  background: rgba(6, 14, 10, 0.55);
  border: 1px solid var(--border);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  color: var(--muted);
  font-size: 11px;
  line-height: 1.6;
  white-space: nowrap;
}
.tab:hover {
  border-color: var(--accent);
}
.tab.active {
  color: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 10px rgba(52, 245, 160, 0.25);
}
.tab-title {
  overflow: hidden;
  text-overflow: ellipsis;
}
.tab-x {
  font-size: 14px;
  line-height: 1;
  opacity: 0.7;
}
.tab-x:hover {
  opacity: 1;
  color: #ff6b6b;
}
.tab.new {
  font-size: 15px;
  padding: 4px 11px;
  color: var(--accent);
}

.transcript {
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition: opacity 0.4s ease;
  /* older lines fade toward the top */
  mask-image: linear-gradient(180deg, transparent, #000 30%);
  -webkit-mask-image: linear-gradient(180deg, transparent, #000 30%);
}
.transcript.dim {
  opacity: 0.55;
}

.line {
  margin: 0;
  font-size: 14px;
  line-height: 1.5;
  color: var(--text);
  text-shadow: 0 1px 12px rgba(0, 0, 0, 0.7);
}
.line .who {
  font-family: var(--mono);
  font-size: 9.5px;
  letter-spacing: 0.18em;
  margin-right: 8px;
  vertical-align: 1px;
}
.line.jarvis .who {
  color: var(--accent);
}
.line.user .who {
  color: var(--muted);
}
.line.user .txt {
  color: var(--muted);
}
.spoke {
  margin-left: 6px;
  opacity: 0.8;
}

/* Rendered Markdown from Jarvis. The transcript is click-through (the backdrop
   breathes underneath), so only the interactive bits (links, images) re-enable
   pointer events. A single-paragraph reply sits inline right after the label; a
   richer reply (lists, code) flows as blocks below it. */
.md {
  display: inline;
}
.md > :first-child {
  margin-top: 0;
}
.md > p:first-child {
  display: inline; /* keep short replies on the label's line */
}
.md p {
  margin: 0 0 6px;
}
.md > :last-child,
.md p:last-child {
  margin-bottom: 0;
}
.md strong {
  color: var(--text);
  font-weight: 700;
}
.md em {
  font-style: italic;
}
.md a {
  color: var(--accent);
  text-decoration: underline;
  text-underline-offset: 2px;
  cursor: pointer;
  pointer-events: auto; /* clickable even though the transcript is click-through */
}
.md code {
  font-family: var(--mono);
  font-size: 0.88em;
  padding: 1px 5px;
  border-radius: 5px;
  background: rgba(255, 255, 255, 0.1);
}
.md pre {
  margin: 6px 0;
  padding: 10px 12px;
  border-radius: 10px;
  background: rgba(0, 0, 0, 0.45);
  border: 1px solid var(--border);
  overflow-x: auto;
}
.md pre code {
  padding: 0;
  background: none;
  font-size: 12px;
  line-height: 1.5;
}
.md ul,
.md ol {
  margin: 6px 0;
  padding-left: 20px;
}
.md li {
  margin: 2px 0;
}
.md blockquote {
  margin: 6px 0;
  padding-left: 10px;
  border-left: 2px solid var(--accent);
  color: var(--muted);
}
.md img {
  display: block;
  max-width: 100%;
  max-height: 240px;
  margin: 6px 0;
  border-radius: 10px;
  border: 1px solid var(--border);
  pointer-events: auto;
}
.md h1,
.md h2,
.md h3 {
  margin: 8px 0 4px;
  font-size: 15px;
  font-weight: 700;
}
.md :is(p, li, h1, h2, h3) {
  word-break: break-word;
  overflow-wrap: anywhere;
}

.dots {
  display: inline-flex;
  gap: 4px;
}
.dots span {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--accent);
  opacity: 0.4;
  animation: cdot 1.1s infinite ease-in-out;
}
.dots span:nth-child(2) {
  animation-delay: 0.18s;
}
.dots span:nth-child(3) {
  animation-delay: 0.36s;
}
@keyframes cdot {
  0%, 60%, 100% { opacity: 0.3; transform: translateY(0); }
  30% { opacity: 1; transform: translateY(-2px); }
}

/* Enter/leave animation for transcript lines. */
.line-enter-active {
  transition: opacity 0.45s ease, transform 0.45s ease;
}
.line-enter-from {
  opacity: 0;
  transform: translateY(8px);
}

/* Bottom-left hover zone. */
.zone {
  position: absolute;
  left: 0;
  bottom: 0;
  width: min(560px, 82vw);
  height: 168px;
  pointer-events: auto;
  padding: 0 clamp(20px, 5vw, 64px) clamp(20px, 4vh, 40px);
  display: flex;
  align-items: flex-end;
}

.peek {
  position: absolute;
  left: clamp(20px, 5vw, 64px);
  bottom: clamp(20px, 4vh, 40px);
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-family: var(--mono);
  font-size: 11px;
  letter-spacing: 0.1em;
  color: var(--muted);
  opacity: 0.7;
  transition: opacity 0.25s ease;
}
.peek :deep(svg) {
  width: 15px;
  height: 15px;
}
.peek.hide {
  opacity: 0;
}

.dock {
  width: 100%;
  transform: translateY(18px);
  opacity: 0;
  pointer-events: none;
  transition: transform 0.3s cubic-bezier(0.22, 1, 0.36, 1), opacity 0.28s ease;
}
.dock.show {
  transform: translateY(0);
  opacity: 1;
  pointer-events: auto;
}

.policy {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  margin-bottom: 9px;
  padding: 4px 10px;
  border-radius: 999px;
  background: rgba(6, 14, 10, 0.55);
  border: 1px solid var(--border);
  backdrop-filter: blur(10px);
  -webkit-backdrop-filter: blur(10px);
  font-family: var(--mono);
  font-size: 10px;
  letter-spacing: 0.03em;
  color: var(--muted);
}
.policy.ok {
  color: var(--accent);
}
.pdot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--muted);
}
.pdot.on {
  background: var(--accent);
  box-shadow: 0 0 8px var(--accent);
}

.row {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 8px;
  border-radius: 16px;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.1), rgba(255, 255, 255, 0.03));
  border: 1px solid rgba(255, 255, 255, 0.14);
  backdrop-filter: blur(24px) saturate(180%);
  -webkit-backdrop-filter: blur(24px) saturate(180%);
  box-shadow: 0 14px 44px rgba(0, 0, 0, 0.5), inset 0 1px 0 rgba(255, 255, 255, 0.28);
}
.row input {
  flex: 1;
  min-width: 0;
  background: transparent;
  border: none;
  color: var(--text);
  font: inherit;
  font-size: 14px;
  padding: 6px 4px;
}
.row input:focus {
  outline: none;
}
.row input::placeholder {
  color: var(--muted);
}

.ic {
  flex: none;
  width: 38px;
  height: 38px;
  border-radius: 11px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: 1px solid var(--border);
  color: var(--text);
}
.ic:hover:not(:disabled) {
  border-color: var(--accent);
}
.ic:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.ic.on {
  color: var(--accent);
  border-color: var(--accent);
  box-shadow: 0 0 8px rgba(52, 245, 160, 0.35);
}
.ic.mic.live {
  border-color: var(--accent);
  color: var(--accent);
}
.ic.send {
  background: var(--accent);
  color: #04140c;
  border: none;
}
.ic :deep(svg) {
  width: 17px;
  height: 17px;
}

/* Touch devices have no hover: keep the input visible and drop the peek hint. */
@media (hover: none) {
  .peek {
    display: none;
  }
  .dock {
    transform: none;
    opacity: 1;
    pointer-events: auto;
  }
}

@media (prefers-reduced-motion: reduce) {
  .dots span,
  .dock,
  .line-enter-active {
    animation: none;
    transition: none;
  }
}
</style>
