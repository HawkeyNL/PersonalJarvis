// Phone-side unlock approvals. This device (typically the phone) polls for
// pending unlock requests from the user's other devices and, on approval, does
// a local biometric check (with device-passcode fallback) and signs the request
// nonce with its device key — the same Ed25519 key used for login.
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { currentSession } from "./auth";
import { getJsonAuth, postJsonAuth } from "./api";

export interface UnlockReq {
  id: string;
  device_name: string;
  platform: string;
  nonce: string;
  created_at: number;
}

export const pending = ref<UnlockReq[]>([]);
export const approving = ref<string | null>(null);
export const approvalError = ref<string | null>(null);

let timer: number | undefined;

export async function refreshPending(): Promise<void> {
  const session = await currentSession();
  if (!session.token) {
    pending.value = [];
    return;
  }
  try {
    const res = await getJsonAuth<{ requests: UnlockReq[] }>(
      "/v1/auth/unlock/pending",
      session.token,
    );
    pending.value = res.requests;
  } catch {
    /* transient — keep the last snapshot */
  }
}

export async function approve(req: UnlockReq): Promise<void> {
  if (approving.value) return;
  approvalError.value = null;
  approving.value = req.id;
  try {
    const session = await currentSession();
    if (!session.token) throw new Error("niet ingelogd");
    // Verify locally on the phone: biometrics, falling back to the passcode.
    await invoke("biometric_unlock", {
      reason: `${req.device_name} ontgrendelen`,
      allowPassword: true,
    });
    // Prove it with the device key by signing the request nonce.
    const signature = await invoke<string>("auth_sign", { nonceHex: req.nonce });
    await postJsonAuth(`/v1/auth/unlock/${req.id}/approve`, session.token, { signature });
    pending.value = pending.value.filter((r) => r.id !== req.id);
  } catch (e) {
    approvalError.value = e instanceof Error ? e.message : String(e);
  } finally {
    approving.value = null;
  }
}

export function startApprovalPolling(): void {
  refreshPending();
  clearInterval(timer);
  timer = window.setInterval(refreshPending, 4000);
}

export function stopApprovalPolling(): void {
  clearInterval(timer);
  timer = undefined;
}
