// Accounts & keys: which actions ask first, and what a key's usage line says.
// No Vue or Tauri imports, so `npm run test:unit` loads this file directly.
import type { AiAccountAction, AiAccountProvider } from "../../admin";
import type { KeyUse } from "../overview/needs.ts";

export interface ConfirmSpec { title: string; detail: string; confirmLabel: string; tone: "primary" | "danger" }

const TERMINAL = "A trusted terminal window opens and asks for system administrator authorization.";

/**
 * The confirmation an account action needs before its terminal opens; null when
 * the action changes nothing yet (Connect when signed out, Test).
 */
export function accountConfirm(provider: AiAccountProvider, label: string, action: AiAccountAction): ConfirmSpec | null {
  if (action === "reconnect") {
    return {
      title: `Reconnect ${label}?`,
      detail: `${TERMINAL} The official ${label} client signs in again there and replaces the current login. Use it when the login expired or should change account.`,
      confirmLabel: `Reconnect ${label}`,
      tone: "primary",
    };
  }
  if (action === "disconnect") {
    return {
      title: `Disconnect ${label}?`,
      detail: `${provider === "codex" ? "Also disables the Codex chat worker socket, which uses this login. " : ""}Active runs may fail. Reconnecting opens a trusted terminal and requires system administrator authorization again.`,
      confirmLabel: `Disconnect ${label}`,
      tone: "danger",
    };
  }
  return null;
}

/** Replacing a stored key asks first; setting a missing one does not. */
export function keyConfirm(label: string, configured: boolean): ConfirmSpec | null {
  if (!configured) return null;
  return {
    title: `Replace the ${label} key?`,
    detail: `${TERMINAL} You enter the new key there, hidden; it never passes through this window. The new key is checked before it replaces the stored one.`,
    confirmLabel: `Replace ${label} key`,
    tone: "primary",
  };
}

const models = (count: number) => `${count} enabled model${count === 1 ? "" : "s"}`;

/** The usage line under a key. `use` undefined: models or routing could not be read. */
export function keyUseLabel(use: KeyUse | null | undefined): string {
  if (use === undefined) return "Usage unknown: models or routing could not be read";
  if (use === null) return "Task classifier key, not used by chat models";
  const users = [use.models ? models(use.models) : "", use.routing ? "routing" : ""].filter(Boolean);
  return users.length ? `Used by ${users.join(" and ")}` : "Not used by enabled models or routing";
}
