// The Overview "Needs you" list: a pure function from what Overview already
// loads to the rows that need the owner. No Vue or Tauri imports, so
// `npm run test:unit` loads this file directly.
import type {
  CredentialRecord, DeviceOverview, LogService, ModelRecord, OverviewResponse, RoutingReport, UsageReport, ViewName,
} from "../../admin";
import { eur } from "../../stats.ts";
import { stateLabel, toneOf } from "../../status.ts";

/** One broker result: data once it arrived, or the error that replaced it. */
export type Source<T> = { data: T | null; error: string };
export interface NeedsSources {
  overview: Source<OverviewResponse>;
  usage: Source<UsageReport>;
  models: Source<ModelRecord[]>;
  routes: Source<RoutingReport>;
  devices: Source<DeviceOverview>;
  credentials: Source<CredentialRecord[]>;
}
/** A row with one action: open a view, or "refresh" to read everything again. */
export interface NeedItem { key: string; tone: "warn" | "error"; title: string; detail: string; action: string; target: ViewName | "refresh" }
export interface Needs { items: NeedItem[]; loading: boolean }

const SOURCE_NAMES: Record<keyof NeedsSources, string> = {
  overview: "services and releases", usage: "usage", models: "models", routes: "routing", devices: "devices", credentials: "credentials",
};

// Status labels the CLI reports for the fixed service allowlist, mapped to the
// fixed log allowlist (admin.ts LogService).
const LOGS: Record<string, LogService> = {
  Core: "core", SurrealDB: "surrealdb", "Config broker": "config-broker", "Codex broker": "codex-broker", OpenSandbox: "opensandbox",
};
/** The log allowlist entry for a service label; null when it has no logs here. */
export const logServiceFor = (label: string): LogService | null => LOGS[label] ?? null;

// Credential ids name model providers differently (credential_setup.rs).
const MODEL_PROVIDER: Record<string, string> = {
  anthropic: "anthropic-api", openai: "openai-api", deepseek: "deepseek-api", xai: "xai-api", zai: "zai-api",
  "ollama-cloud": "ollama-cloud", huggingface: "huggingface",
};

// Core retries the usage aggregate while it starts; that is not a failure.
export const usageInitializing = (error: string) => error.includes("usage statistics are unavailable");

// `systemctl is-enabled` exits non-zero for a disabled timer and the CLI then
// reports "inactive". A timer off is the owner's choice, so it stays neutral.
const UPDATER: Record<string, string> = { disabled: "Manual · timer off", inactive: "Manual · timer off", enabled: "Automatic · timer on" };
/** Label for the updater timer state; null when it is not a known state. */
export const updaterLabel = (state: string): string | null => UPDATER[state] ?? null;

const tag = (value = "") => /^v?(\d+)\.(\d+)\.(\d+)$/.exec(value)?.slice(1).map(Number) ?? null;
/** True when release tag `latest` is newer than `current`. Malformed tags never are. */
export function newerRelease(latest?: string, current?: string): boolean {
  const a = tag(latest), b = tag(current);
  if (!a || !b) return false;
  const i = a.findIndex((part, n) => part !== b[n]);
  return i >= 0 && a[i] > b[i];
}

/** Services whose state is anything but active. */
const inactiveServices = (services: Record<string, string>): [string, string][] =>
  Object.entries(services).filter(([, state]) => state.toLowerCase() !== "active");

export function needsYou(sources: NeedsSources, credentialLabels: Record<string, string>): Needs {
  const items: NeedItem[] = [];
  const { overview, usage, models, routes, devices, credentials } = sources;

  for (const [name, state] of inactiveServices(overview.data?.status.services ?? {})) {
    const logs = logServiceFor(name);
    items.push({
      key: `service:${name}`, tone: toneOf(state) === "error" ? "error" : "warn",
      title: `${name} is not active`,
      // ponytail: Logs opens on Core; preselecting the unit needs a LogsView prop.
      detail: `State: ${stateLabel(state)}.${logs && logs !== "core" ? ` In Logs, choose ${name}.` : ""}`,
      action: logs ? "Open logs" : "Refresh", target: logs ? "logs" : "refresh",
    });
  }
  if (usage.data?.over_budget) {
    items.push({
      key: "budget", tone: "error", title: "Over the monthly model budget",
      detail: `${eur(usage.data.spent_eur)} of ${eur(usage.data.budget_eur)} spent this month.`,
      action: "Open usage", target: "usage",
    });
  }
  const reason = routes.data?.routing_unavailable_reason;
  if (reason) {
    items.push({
      key: "routing", tone: "warn", title: "Routing file needs repair",
      detail: `${reason} Paid APIs stay off until it is repaired.`,
      action: "Open routing", target: "models",
    });
  }
  for (const request of devices.data?.requests ?? []) {
    items.push({
      key: `pairing:${request.id}`, tone: "warn", title: `${request.name} wants to pair`,
      detail: `Platform: ${request.platform}. Check its fingerprint before you approve it.`,
      action: "Review", target: "devices",
    });
  }
  const update = overview.data?.update;
  const latest = update?.latest ? ` ${update.latest}` : "";
  // A schema change needs the explicit migration; a routine update refuses it.
  if (update?.schema === "migration required") {
    items.push({
      key: "migration", tone: "warn", title: `Schema migration available${latest ? ` for${latest}` : ""}`,
      detail: "This release changes the database schema and installs only through the explicit migration.",
      action: "Open releases", target: "releases",
    });
  // Overview reads the status without a check, so the tags decide.
  } else if (update?.update === "available" || newerRelease(update?.latest, update?.current)) {
    items.push({
      key: "update", tone: "warn", title: `Update available${latest ? `:${latest}` : ""}`,
      detail: "A newer release is ready to install. Updates are manual.",
      action: "Open releases", target: "releases",
    });
  }
  // A key is missing only when something uses its provider: an enabled model
  // or a routing tier. Unused providers stay quiet.
  const used = new Set([
    ...(models.data ?? []).filter((row) => row.enabled).map((row) => row.provider),
    ...Object.values(routes.data?.routing?.tiers ?? {}).flatMap((tier) => tier?.chain.map((entry) => entry.provider) ?? []),
  ]);
  for (const row of credentials.data ?? []) {
    if (row.configured || !used.has(MODEL_PROVIDER[row.provider])) continue;
    const label = credentialLabels[row.provider] ?? row.provider;
    items.push({
      key: `key:${row.provider}`, tone: "warn", title: `${label} key is not set`,
      detail: `Enabled models or routing use ${label}; those calls fail without a key.`,
      action: "Set key", target: "credentials",
    });
  }

  // Without every source, "All good" would be a guess: name what is missing.
  // A failed refresh counts too, since the data it kept may be stale.
  const entries = Object.entries(sources) as [keyof NeedsSources, Source<unknown>][];
  const failed = entries.filter(([name, source]) => source.error && !(name === "usage" && usageInitializing(source.error))).map(([name]) => SOURCE_NAMES[name]);
  if (failed.length) {
    items.push({
      key: "unread", tone: "warn", title: "Some checks could not run",
      detail: `Could not read ${failed.join(", ")}.`,
      action: "Refresh", target: "refresh",
    });
  }
  return { items, loading: entries.some(([, source]) => !source.data && !source.error) };
}
