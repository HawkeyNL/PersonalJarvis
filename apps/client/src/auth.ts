// Client-side device-bound login.
//
// The private key and signing live in Rust (Tauri commands); this module only
// orchestrates the HTTP flow (enroll -> challenge -> login) and reads back the
// session state. The private key never enters JS.
import { invoke } from "@tauri-apps/api/core";
import { getJsonAuth, postAuth, postJson } from "./api";

export type Session = {
  device_id: string | null;
  token: string | null;
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

/// Enroll (first time) and log in, storing the session token natively.
export async function login(): Promise<void> {
  const publicKey = await invoke<string>("auth_public_key");
  const info = await invoke<{ platform: string; name: string }>("device_info");

  let session = await currentSession();
  let deviceId = session.device_id;

  if (!deviceId) {
    const enroll = await postJson<{ device_id: string }>("/v1/auth/enroll", {
      name: info.name,
      platform: info.platform,
      public_key: publicKey,
    });
    deviceId = enroll.device_id;
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

export async function listDevices(token: string): Promise<DeviceItem[]> {
  const res = await getJsonAuth<{ devices: DeviceItem[] }>("/v1/devices", token);
  return res.devices;
}
