// Small API client. Public enrollment calls use the Tauri HTTP plugin; calls
// that need a bearer go through the native auth_request command. The enrolled
// Home Node origin is resolved at request time, and production builds contain
// no infrastructure URL.
import { invoke } from "@tauri-apps/api/core";
import { fetch } from "@tauri-apps/plugin-http";
import { homeNodeOrigin } from "./homeNode";

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
  const origin = await homeNodeOrigin();
  const controller = new AbortController();
  let timedOut = false;
  const timeout = window.setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, REQUEST_TIMEOUT_MS);
  const abort = () => controller.abort();
  init.signal?.addEventListener("abort", abort, { once: true });
  try {
    return await fetch(`${origin}${path}`, { ...init, signal: controller.signal });
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

type NativeApiResponse = {
  status: number;
  body: unknown | null;
};

async function authenticatedRequest<T>(
  method: "GET" | "POST" | "DELETE",
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  let response: NativeApiResponse;
  try {
    const pending = invoke<NativeApiResponse>("auth_request", {
      method,
      path,
      body: body ?? null,
    });
    response = signal
      ? await Promise.race([
          pending,
          new Promise<never>((_, reject) => {
            if (signal.aborted) reject(new DOMException("Aborted", "AbortError"));
            else signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
          }),
        ])
      : await pending;
  } catch {
    throw new NetworkError("unreachable", path);
  }
  if (response.status < 200 || response.status >= 300) {
    throw new ApiError(response.status, path);
  }
  return response.body as T;
}

export function getJsonAuth<T>(path: string): Promise<T> {
  return authenticatedRequest<T>("GET", path);
}

export async function postAuth(path: string): Promise<void> {
  await authenticatedRequest<void>("POST", path, {});
}

export async function postJsonAuth<T>(
  path: string,
  body: unknown,
  signal?: AbortSignal,
): Promise<T> {
  return authenticatedRequest<T>("POST", path, body, signal);
}

export async function deleteAuth(path: string): Promise<void> {
  await authenticatedRequest<void>("DELETE", path);
}
