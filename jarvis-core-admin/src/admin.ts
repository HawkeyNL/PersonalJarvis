import { invoke } from "@tauri-apps/api/core";

export type ViewName =
  | "overview"
  | "health"
  | "services"
  | "update"
  | "agents"
  | "models"
  | "usage"
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
  profile_lines: number | null;
  source_updated_at: string | null;
  state: string;
}
export interface AgentsResponse {
  bundle: AgentBundle;
  manifest_bundle: string | null;
  agents: AgentRecord[];
}
export interface ModelRecord {
  provider: string;
  model: string;
  enabled: boolean;
  source: string;
  price_status: "known" | "unknown" | "local";
  input_per_million_usd: number | null;
  cache_read_per_million_usd: number | null;
  output_per_million_usd: number | null;
  pricing_source: string;
  pricing_updated_at: string;
}
export interface UsageRow {
  backend: string;
  model: string | null;
  spent_eur: number;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
}
export interface DailyUsageRow extends Omit<UsageRow, "backend" | "model"> { day: string }
export interface UsageReport {
  period: string;
  generated_at_unix: number;
  budget_eur: number;
  spent_eur: number;
  remaining_eur: number;
  over_budget: boolean;
  reserved_eur: number;
  remaining_hard_eur: number;
  above_soft_budget: boolean;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
  by_backend: UsageRow[];
  by_model: UsageRow[];
  daily: DailyUsageRow[];
  pricing: { source: string; updated_at: string };
}
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
  usage: () => invoke<UsageReport>("usage"),
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
  return ["active", "enabled", "passed", "healthy", "configured", "known", "local"].includes(
    state.toLowerCase(),
  );
}
