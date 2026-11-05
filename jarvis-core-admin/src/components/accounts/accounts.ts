// Accounts & keys: which actions ask first, and what a key's usage line says.
// No Vue or Tauri imports, so `npm run test:unit` loads this file directly.
import type { AiAccountAction, AiAccountProvider, AiAccountRecord, AiWorkerAction } from "../../admin";
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
      detail: `${TERMINAL} The official ${label} client signs in again there. If that sign-in completes, it replaces the current login. Use it to switch accounts or to refresh a login that stopped working.`,
      confirmLabel: `Reconnect ${label}`,
      tone: "primary",
    };
  }
  if (action === "disconnect") {
    return {
      title: `Disconnect ${label}?`,
      detail: `${TERMINAL} ${provider === "codex" ? "It disables the Codex chat worker, broker and coding service, which use this login," : "It disables the Claude worker socket and stops active Claude runs,"} then signs out; the terminal asks you to type YES first. Connecting again needs a new sign-in.`,
      confirmLabel: `Disconnect ${label}`,
      tone: "danger",
    };
  }
  return null;
}

/**
 * The worker-socket action a subscription card offers: enable for a connected
 * Claude login whose socket is off, disable while the socket or a run is up.
 */
export function workerAction(row: Pick<AiAccountRecord, "provider" | "state" | "runtime">): AiWorkerAction | null {
  if (row.provider !== "claude") return null;
  if (row.runtime === "socket_ready" || row.runtime === "active") return "disable";
  if (row.state === "connected" && row.runtime === "inactive") return "enable";
  return null;
}

const AUTHORIZE = "Your system asks for administrator authorization.";

/** Both worker actions ask first; enabling is the one that can cost money. */
export function workerConfirm(action: AiWorkerAction): ConfirmSpec {
  if (action === "enable") {
    return {
      title: "Enable the Claude worker?",
      detail: `Only continue if you turned off extra usage on claude.ai (or set its limit to 0). Jarvis cannot verify that setting; without it, Jarvis runs can be billed beyond your subscription. ${AUTHORIZE} The login and runtime are checked again before the worker socket starts. CLI equivalent: sudo jarvis accounts worker enable claude`,
      confirmLabel: "Enable worker",
      tone: "danger",
    };
  }
  return {
    title: "Disable the Claude worker?",
    detail: `This disables the Claude worker socket and stops active Claude runs. The login stays connected. ${AUTHORIZE} CLI equivalent: sudo jarvis accounts worker disable claude`,
    confirmLabel: "Disable worker",
    tone: "danger",
  };
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
