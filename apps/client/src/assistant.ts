// Conversation state with Jarvis. The reasoning/brain is not wired yet
// (DEC-001 — LLM provider), so replies are a deliberate placeholder; the
// chat, voice-output policy and TTS are real. See assistant docs.
import { ref } from "vue";
import { speak } from "./voice";

export type Role = "user" | "jarvis";
export interface Msg {
  id: number;
  role: Role;
  text: string;
  ts: string;
  spoken: boolean;
}

export const messages = ref<Msg[]>([]);
let idc = 0;

function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

function push(role: Role, text: string, spoken = false) {
  messages.value.push({ id: idc++, role, text, ts: stamp(), spoken });
}

// Placeholder brain until the LLM provider (DEC-001) is connected.
function draftReply(input: string): string {
  return (
    `Genoteerd: “${input}”. Mijn taalmodel is nog niet gekoppeld (DEC-001), ` +
    `dus dit is een voorlopig antwoord — de chat, spraak en uitvoer-policy werken al.`
  );
}

/** Send a user message; Jarvis replies (and speaks if the policy allows). */
export function send(input: string): void {
  const t = input.trim();
  if (!t) return;
  push("user", t);
  const reply = draftReply(t);
  const spoken = speak(reply); // speaks only when canSpeak() allows
  push("jarvis", reply, spoken);
}
