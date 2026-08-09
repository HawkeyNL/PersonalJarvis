// Small API client. Uses the Tauri HTTP plugin so requests are routed through
// Rust (no webview CORS restrictions). The base URL is configurable so a real
// device can later point at the Mac's LAN address instead of localhost.
import { fetch } from "@tauri-apps/plugin-http";

export const API_BASE = "http://localhost:8080";

/** An HTTP error carrying the status code, so callers can react to e.g. 401. */
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    path: string,
  ) {
    super(`HTTP ${status} (${path})`);
    this.name = "ApiError";
  }
}

export async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, { method: "GET" });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function postJson<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function getJsonAuth<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function postAuth(path: string, token: string): Promise<void> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
}

export async function postJsonAuth<T>(
  path: string,
  token: string,
  body: unknown,
): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function deleteAuth(path: string, token: string): Promise<void> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
}
