// Pending device pairing approvals. The local OS authenticates the owner before
// the native layer signs the narrowly-scoped pairing protocol.
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { currentSession } from "./auth";
import { getJsonAuth, postJsonAuth } from "./api";

export type PairingRequest = {
  id: string;
  device_name: string;
  platform: string;
  fingerprint: string;
  nonce: string;
  candidate_public_key: string;
  created_at: number;
  expires_at: number;
};
export const pairingRequests = ref<PairingRequest[]>([]);
export const pairingError = ref<string | null>(null);
let polling = false;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function refresh(): Promise<boolean> {
  const session = await currentSession();
  if (!session.token) return false;
  try {
    const response = await getJsonAuth<{ requests: PairingRequest[] }>("/v1/auth/pairing/requests", session.token);
    pairingRequests.value = response.requests;
    return true;
  } catch { return false; }
}

export async function approvePairing(request: PairingRequest): Promise<void> {
  const session = await currentSession();
  if (!session.token || !session.device_id) throw new Error("niet ingelogd");
  pairingError.value = null;
  try {
    await invoke("biometric_unlock", { reason: `${request.device_name} koppelen`, allowPassword: true });
    const signature = await invoke<string>("auth_sign_pairing_approval", {
      requestId: request.id, nonceHex: request.nonce,
      candidatePublicKeyHex: request.candidate_public_key,
      userId: await ownerId(), approverDeviceId: session.device_id, expiresAt: request.expires_at,
    });
    await postJsonAuth(`/v1/auth/pairing/requests/${request.id}/approve`, session.token, { signature });
    pairingRequests.value = pairingRequests.value.filter((item) => item.id !== request.id);
  } catch (error) { pairingError.value = error instanceof Error ? error.message : String(error); }
}

// The API intentionally owns the user binding. Devices learn it from the
// authenticated session endpoint rather than a candidate-supplied field.
async function ownerId(): Promise<string> {
  const session = await currentSession();
  const response = await getJsonAuth<{ user_id: string }>("/v1/auth/me", session.token!);
  return response.user_id;
}

export async function denyPairing(request: PairingRequest): Promise<void> {
  const session = await currentSession();
  if (!session.token) throw new Error("niet ingelogd");
  await postJsonAuth(`/v1/auth/pairing/requests/${request.id}/deny`, session.token, {});
  pairingRequests.value = pairingRequests.value.filter((item) => item.id !== request.id);
}

export function startPairingPolling(): void {
  if (polling) return; polling = true;
  (async () => { while (polling) { await refresh(); await sleep(10_000); } })();
}
export function stopPairingPolling(): void { polling = false; }
