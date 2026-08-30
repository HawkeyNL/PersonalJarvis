import { invoke } from "@tauri-apps/api/core";

export type ViewName =
  | "overview"
  | "health"
  | "services"
  | "update"
  | "agents"
  | "models"
  | "credentials"
  | "logs"
  | "system";

export interface SessionStatus {
  authenticated: boolean;
  expires_in_seconds: number;
}

export interface AgentBundle { id: string; agent_count: number }
export interface StatusReport {
  release: string | null;
  services: Record<string, string>;
  updater_enabled: string;
  agent_bundle: AgentBundle | null;
}
export interface OverviewResponse {
  status: StatusReport;
  update: Record<string, string> | null;
}
export interface HealthResponse {
  checks: Record<string, string>;
  verification: string | null;
}
export interface ServiceRecord { name: string; state: string }
export interface OperationResult { success: boolean; summary: string; detail: string }
export interface AgentRecord {
  id: string;
  name: string;
  group: string;
  model_policy: string | null;
  state: string;
}
export interface AgentsResponse {
  bundle: AgentBundle;
  manifest_bundle: string | null;
  agents: AgentRecord[];
}
export interface ModelRecord { provider: string; model: string; enabled: boolean; source: string }
export interface CredentialRecord { provider: string; configured: boolean }
export type LogService =
  | "core"
  | "surrealdb"
  | "config-broker"
  | "codex-broker"
  | "opensandbox"
  | "updater"
  | "agents-updater";
export interface LogRecord {
  id: number;
  timestamp: string | null;
  level: string;
  message: string;
  target: string | null;
  details: [string, string][];
}
export interface LogResponse { unit: string; records: LogRecord[] }
export interface SystemResponse { values: [string, string][] }

export const api = {
  sessionAuthenticate: () =>
    invoke<SessionStatus>("session_authenticate"),
  sessionTouch: () => invoke<SessionStatus>("session_touch"),
  sessionLock: () => invoke<SessionStatus>("session_lock"),
  overview: () => invoke<OverviewResponse>("overview"),
  health: (runVerification = false) =>
    invoke<HealthResponse>("health", { runVerification }),
  services: () => invoke<ServiceRecord[]>("services"),
  updateStatus: (check = false) =>
    invoke<Record<string, string>>("update_status", { check }),
  updateMutation: (request: Record<string, string>) =>
    invoke<OperationResult>("update_mutation", { request }),
  agents: () => invoke<AgentsResponse>("agents"),
  agentAction: (update: boolean) =>
    invoke<OperationResult>("agent_action", { update }),
  models: () => invoke<ModelRecord[]>("models"),
  modelMutation: (request: Record<string, string>) =>
    invoke<OperationResult>("model_mutation", { request }),
  credentials: () => invoke<CredentialRecord[]>("credentials"),
  logs: (service: LogService, lines = 500) =>
    invoke<LogResponse>("logs", { query: { service, lines } }),
  system: () => invoke<SystemResponse>("system"),
};

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "The trusted administration operation failed.";
}

export function healthy(state: string): boolean {
  return ["active", "enabled", "passed", "healthy", "configured"].includes(
    state.toLowerCase(),
  );
}
