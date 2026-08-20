// Conversation list + current-tab state (ADR-030). The chats themselves live
// server-side (Jarvis stores every turn), so they survive an app restart and
// follow you across devices; this store just mirrors the list and remembers
// which tab you had open. The messages of the *current* conversation live in
// `assistant.ts` (which imports from here — this module must not import it back,
// to avoid a cycle).
import { ref } from "vue";
import { currentSession } from "./auth";
import { getJsonAuth, deleteAuth } from "./api";

export interface ConversationSummary {
  id: string;
  title: string;
  updated_at: string;
}

export const conversations = ref<ConversationSummary[]>([]);
export const currentId = ref<string | null>(null);

const LS_CURRENT = "jarvis.conversation.current";

/** Remember which conversation is open, so a restart reopens the same tab. */
export function setCurrent(id: string | null): void {
  currentId.value = id;
  if (id) localStorage.setItem(LS_CURRENT, id);
  else localStorage.removeItem(LS_CURRENT);
}

export function savedCurrentId(): string | null {
  return localStorage.getItem(LS_CURRENT);
}

/** Refresh the tab list from the server (newest-active first). */
export async function loadConversations(): Promise<void> {
  const session = await currentSession();
  if (!session.token) return;
  const res = await getJsonAuth<{ conversations: ConversationSummary[] }>(
    "/v1/conversations",
    session.token,
  );
  conversations.value = res.conversations;
}

/** Delete a conversation and drop it from the list. Returns the id that should
 *  become current afterward (first remaining, or null), for the caller to open. */
export async function deleteConversation(id: string): Promise<string | null> {
  const session = await currentSession();
  if (!session.token) return currentId.value;
  await deleteAuth(`/v1/conversations/${id}`, session.token);
  conversations.value = conversations.value.filter((c) => c.id !== id);
  if (currentId.value === id) {
    const next = conversations.value[0]?.id ?? null;
    setCurrent(next);
    return next;
  }
  return currentId.value;
}
