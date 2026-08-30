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

export type NetworkFailureKind = "offline" | "timeout" | "unreachable";

export class NetworkError extends Error {
  constructor(
    public readonly kind: NetworkFailureKind,
    path: string,
  ) {
    super(`${kind} (${path})`);
    this.name = "NetworkError";
  }
}

const REQUEST_TIMEOUT_MS = 10_000;

async function request(path: string, init: RequestInit = {}): Promise<Response> {
  const controller = new AbortController();
  let timedOut = false;
  const timeout = window.setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, REQUEST_TIMEOUT_MS);
  const abort = () => controller.abort();
  init.signal?.addEventListener("abort", abort, { once: true });
  try {
    return await fetch(`${API_BASE}${path}`, { ...init, signal: controller.signal });
  } catch (error) {
    if (init.signal?.aborted) throw error;
    if (timedOut) throw new NetworkError("timeout", path);
    if (typeof navigator !== "undefined" && !navigator.onLine) {
      throw new NetworkError("offline", path);
    }
    throw new NetworkError("unreachable", path);
  } finally {
    window.clearTimeout(timeout);
    init.signal?.removeEventListener("abort", abort);
  }
}

export async function getJson<T>(path: string): Promise<T> {
  const res = await request(path, { method: "GET" });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function getJsonWithHeaders<T>(path: string, headers: Record<string, string>): Promise<T> {
  const res = await request(path, { method: "GET", headers });
  if (!res.ok) throw new ApiError(res.status, path);
  return (await res.json()) as T;
}

export async function postJson<T>(path: string, body: unknown): Promise<T> {
  const res = await request(path, {
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
  const res = await request(path, { method: "POST", headers: { "Content-Type": "application/json", ...headers }, body: JSON.stringify(body) });
  if (!res.ok) throw new ApiError(res.status, path);
  return (await res.json()) as T;
}

export async function getJsonAuth<T>(path: string, token: string): Promise<T> {
  const res = await request(path, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
  return (await res.json()) as T;
}

export async function postAuth(path: string, token: string): Promise<void> {
  const res = await request(path, {
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
  const res = await request(path, {
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
  const res = await request(path, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) {
    throw new ApiError(res.status, path);
  }
}
