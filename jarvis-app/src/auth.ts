// Client-side device-bound login.
//
// The private key and signing live in Rust (Tauri commands); this module only
// orchestrates the HTTP flow (enroll -> challenge -> login) and reads back the
// session state. The private key never enters JS.
import { invoke } from "@tauri-apps/api/core";
import { deleteAuth, getJsonAuth, getJsonWithHeaders, postAuth, postJson, postJsonWithHeaders } from "./api";

export type Session = {
  device_id: string | null;
  token: string | null;
  has_key: boolean;
};

export type AuthStatus = {
  device_id: string | null;
  authenticated: boolean;
  has_key: boolean;
};

export type DeviceItem = {
  id: string;
  name: string;
  platform: string;
  status: string;
};

export function currentSession(): Promise<Session> {
  return invoke<Session>("auth_session");
}

export function currentAuthStatus(): Promise<AuthStatus> {
  return invoke<AuthStatus>("auth_status");
}

const PAIRING_WAIT_KEY = "jarvis.pairing.wait";
type PairingWait = { request_id: string; nonce: string; expires_at: number };
export class PairingPending extends Error {
  constructor() { super("Wacht op goedkeuring vanaf een vertrouwd Jarvis-apparaat."); }
}

/// Log in with the local device key. An unknown device creates one bounded
/// pairing request and waits; it can never self-enrol through a session token.
export async function login(): Promise<void> {
  const publicKey = await invoke<string>("auth_public_key");
  const info = await invoke<{ platform: string; name: string }>("device_info");

  let session = await currentSession();
  let deviceId = session.device_id;

  if (!deviceId) {
    const stored = sessionStorage.getItem(PAIRING_WAIT_KEY);
    if (stored) {
      const waiting = JSON.parse(stored) as PairingWait;
      const status = await getJsonWithHeaders<{ status: string; device_id: string | null }>(
        `/v1/auth/pairing/requests/${waiting.request_id}/status`,
        { "X-Jarvis-Pairing-Nonce": waiting.nonce },
      );
      if (status.status === "approved" && status.device_id) {
        deviceId = status.device_id;
        sessionStorage.removeItem(PAIRING_WAIT_KEY);
      } else if (status.status === "pending") {
        throw new PairingPending();
      } else {
        sessionStorage.removeItem(PAIRING_WAIT_KEY);
        throw new Error("pairing request is verlopen of afgewezen");
      }
    } else {
      const pairing = await postJson<PairingWait>("/v1/auth/pairing/requests", {
        name: info.name, platform: info.platform, public_key: publicKey,
      });
      sessionStorage.setItem(PAIRING_WAIT_KEY, JSON.stringify(pairing));
      throw new PairingPending();
    }
  }

  const challenge = await postJson<{ challenge_id: string; nonce: string }>(
    "/v1/auth/challenge",
    { device_id: deviceId },
  );
  const signature = await invoke<string>("auth_sign", {
    nonceHex: challenge.nonce,
  });
  const result = await postJson<{ token: string }>("/v1/auth/login", {
    device_id: deviceId,
    challenge_id: challenge.challenge_id,
    signature,
  });

  await invoke("auth_save", { deviceId, token: result.token });
}

/** Local-LAN first-owner bootstrap. The secret is used once, never persisted,
 * and is expected to come from the root-operated Home Node provisioning flow. */
export async function bootstrapFirstDevice(secret: string): Promise<void> {
  const publicKey = await invoke<string>("auth_public_key");
  const info = await invoke<{ platform: string; name: string }>("device_info");
  const enrolled = await postJsonWithHeaders<{ device_id: string }>("/v1/auth/bootstrap", {
    name: info.name, platform: info.platform, public_key: publicKey,
  }, { "X-Jarvis-Bootstrap-Secret": secret });
  await invoke("auth_save", { deviceId: enrolled.device_id, token: "" });
  await login();
}

/** Drop the locally stored session token (keeps the enrolled device + key), so
 *  the next `login()` mints a fresh session. Used to recover from a stale token
 *  (e.g. the backend restarted and forgot the session) instead of looping on 401. */
export async function clearSession(): Promise<void> {
  await invoke("auth_logout");
}

export async function logout(): Promise<void> {
  const session = await currentSession();
  if (session.token) {
    // Best-effort server-side revocation; clear locally regardless.
    try {
      await postAuth("/v1/auth/logout", session.token);
    } catch {
      /* ignore */
    }
  }
  await invoke("auth_logout");
}

/** Fully unlink this device: revoke it server-side (invalidating its sessions)
 *  and wipe the local key/id/token. Destructive — the next login() enrolls a
 *  brand-new device. Best-effort on the server call; always clears locally. */
export async function deregisterDevice(): Promise<void> {
  const session = await currentSession();
  if (session.token && session.device_id) {
    try {
      await deleteAuth(`/v1/devices/${session.device_id}`, session.token);
    } catch {
      /* revoke best-effort; still wipe locally so this device is detached */
    }
  }
  await invoke("auth_reset");
}

export async function listDevices(token: string): Promise<DeviceItem[]> {
  const res = await getJsonAuth<{ devices: DeviceItem[] }>("/v1/devices", token);
  return res.devices;
}
