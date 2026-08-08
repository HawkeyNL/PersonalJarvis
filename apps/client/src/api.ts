// Small API client. Uses the Tauri HTTP plugin so requests are routed through
// Rust (no webview CORS restrictions). The base URL is configurable so a real
// device can later point at the Mac's LAN address instead of localhost.
import { fetch } from "@tauri-apps/plugin-http";

export const API_BASE = "http://localhost:8080";

export async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, { method: "GET" });
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}
