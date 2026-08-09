// Conversation state with Jarvis. The brain is the backend `/v1/assistant/chat`
// endpoint (DEC-001 = Claude, provider-abstracted with an Ollama fallback). The
// API key lives only in the backend — never here. The chat, voice-output policy
// and TTS are handled client-side.
import { ref } from "vue";
import { speak } from "./voice";
import { currentSession } from "./auth";
import { postJsonAuth } from "./api";

export type Role = "user" | "jarvis";
export interface Msg {
  id: number;
  role: Role;
  text: string;
  ts: string;
  spoken: boolean;
}

export const messages = ref<Msg[]>([]);
export const thinking = ref(false); // true while the brain is generating a reply
let idc = 0;

function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

function push(role: Role, text: string, spoken = false) {
  messages.value.push({ id: idc++, role, text, ts: stamp(), spoken });
}

interface ChatReply {
  reply: string;
  model: string | null;
  stop_reason: string | null;
}

// Ask the backend brain, sending the conversation so far. The persona/system
// prompt is prepended server-side, so we only send the raw turns.
// Only send the most recent turns so a long chat can't grow the request (and
// token cost) without bound. The system prompt is added server-side.
const MAX_TURNS = 20;

async function ask(): Promise<ChatReply> {
  const session = await currentSession();
  if (!session.token) throw new Error("niet ingelogd");
  const history = messages.value.slice(-MAX_TURNS).map((m) => ({
    role: m.role === "jarvis" ? "assistant" : "user",
    content: m.text,
  }));
  return await postJsonAuth<ChatReply>("/v1/assistant/chat", session.token, {
    messages: history,
  });
}

/** Send a user message; Jarvis replies (and speaks if the policy allows). */
export async function send(input: string): Promise<void> {
  const t = input.trim();
  if (!t) return;
  push("user", t);
  thinking.value = true;
  try {
    const res = await ask();
    const spoken = speak(res.reply); // speaks only when canSpeak() allows
    push("jarvis", res.reply, spoken);
  } catch (e) {
    const detail = e instanceof Error ? e.message : "onbekende fout";
    push(
      "jarvis",
      `Mijn brein is even niet bereikbaar (${detail}). Controleer JARVIS_LLM_API_KEY in de backend of start Ollama lokaal.`,
      false,
    );
  } finally {
    thinking.value = false;
  }
}
