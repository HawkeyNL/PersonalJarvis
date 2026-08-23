// Small API client. Uses the Tauri HTTP plugin so requests are routed through
// Rust (no webview CORS restrictions). Configure a release with
// VITE_JARVIS_API_BASE=https://api.example.com; development remains local.
import { fetch } from "@tauri-apps/plugin-http";

const configuredApiBase = import.meta.env.VITE_JARVIS_API_BASE?.trim();
export const API_BASE = (configuredApiBase || "http://localhost:8080").replace(/\/$/, "");

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

export async function getJsonWithHeaders<T>(path: string, headers: Record<string, string>): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, { method: "GET", headers });
  if (!res.ok) throw new ApiError(res.status, path);
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

export async function postJsonWithHeaders<T>(path: string, body: unknown, headers: Record<string, string>): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, { method: "POST", headers: { "Content-Type": "application/json", ...headers }, body: JSON.stringify(body) });
  if (!res.ok) throw new ApiError(res.status, path);
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
  signal?: AbortSignal,
): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(body),
    signal,
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
