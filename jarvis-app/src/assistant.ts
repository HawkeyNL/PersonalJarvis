// Conversation state with Jarvis. The brain is the backend `/v1/assistant/chat`
// endpoint (DEC-001 = Claude, provider-abstracted with an Ollama fallback). The
// API key lives only in the backend — never here.
//
// Chats are persisted server-side and grouped into conversations ("tabs",
// ADR-030): every turn is stored, and when you switch topic Jarvis splits the
// thread into a new conversation. This module owns the *current* conversation's
// messages; the tab list lives in `conversations.ts`.
import { ref } from "vue";
import { speak } from "./voice";
import { currentAuthStatus } from "./auth";
import { postJsonAuth, getJsonAuth } from "./api";
import {
  currentId,
  setCurrent,
  savedCurrentId,
  loadConversations,
  conversations,
} from "./conversations";

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
  conversation_id?: string;
  conversation_title?: string;
  new_topic?: boolean;
}

// Only send the most recent turns so a long chat can't grow the request (and
// token cost) without bound. The system prompt is added server-side.
const MAX_TURNS = 20;

async function ask(): Promise<ChatReply> {
  const status = await currentAuthStatus();
  if (!status.authenticated) throw new Error("niet ingelogd");
  const history = messages.value.slice(-MAX_TURNS).map((m) => ({
    role: m.role === "jarvis" ? "assistant" : "user",
    content: m.text,
  }));
  return await postJsonAuth<ChatReply>("/v1/assistant/chat", {
    messages: history,
    conversation_id: currentId.value,
  });
}

/** Load a conversation's history into the view and make it the current tab. */
export async function openConversation(id: string): Promise<void> {
  setCurrent(id);
  const status = await currentAuthStatus();
  if (!status.authenticated) return;
  const res = await getJsonAuth<{
    id: string;
    title: string;
    messages: { role: string; content: string; model: string | null; at: string }[];
  }>(`/v1/conversations/${id}`);
  messages.value = res.messages.map((m) => ({
    id: idc++,
    role: m.role === "assistant" ? "jarvis" : "user",
    text: m.content,
    ts: m.at?.slice(11) || stamp(), // "YYYY-MM-DD HH:MM" → "HH:MM"
    spoken: false,
  }));
}

/** Start a fresh conversation: the next message opens a new tab server-side. */
export function startNewConversation(): void {
  setCurrent(null);
  messages.value = [];
}

/** On launch, restore the tab list and reopen the last (or most recent) chat,
 *  so the conversation is right there after an app restart. */
export async function initChat(): Promise<void> {
  try {
    await loadConversations();
    const saved = savedCurrentId();
    const exists = saved && conversations.value.some((c) => c.id === saved);
    const target = exists ? saved! : (conversations.value[0]?.id ?? null);
    if (target) await openConversation(target);
    else startNewConversation();
  } catch {
    // Offline or not logged in yet — leave the chat empty; it'll load later.
  }
}

/** Send a user message; Jarvis replies (and speaks if the policy allows). */
export async function send(input: string): Promise<void> {
  const t = input.trim();
  if (!t) return;
  push("user", t);
  thinking.value = true;
  const hadHistory = messages.value.length > 1;
  try {
    const res = await ask();
    const spoken = speak(res.reply); // speaks only when canSpeak() allows

    // The server may have placed this turn in a different conversation: the very
    // first message (no tab yet) or a mid-chat topic split. Follow it.
    const moved = !!res.conversation_id && res.conversation_id !== currentId.value;
    if (moved) setCurrent(res.conversation_id!);

    if (res.new_topic && moved && hadHistory) {
      // A topic split: reload the new thread so the tab shows only its own turns.
      await openConversation(res.conversation_id!);
    } else {
      push("jarvis", res.reply, spoken);
    }
    // Refresh the tab list (new tab / updated title + order).
    void loadConversations();
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
