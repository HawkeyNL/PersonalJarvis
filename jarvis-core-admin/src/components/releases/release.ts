// Release helpers for the Releases view: version labels, the CLI equivalent
// and the record of a long operation. No Vue or Tauri imports, so
// `npm run test:unit` loads this file directly.
import type { OperationResult } from "../../admin";
import type { Tone } from "../../status";
import { newerRelease } from "../overview/needs.ts";

const TAG = /^v(\d{1,9})\.(\d{1,9})\.(\d{1,9})$/;

/** A `vMAJOR.MINOR.PATCH` release tag, or "" for "unavailable" and anything else. */
export const releaseTag = (value?: string): string => (value && TAG.test(value) ? value : "");

export interface ReleaseOffer {
  current: string;
  previous: string;
  /** The release to offer, or "" when nothing newer is known. */
  install: string;
  /** The release that needs an explicit schema migration, or "". */
  migrate: string;
  /** The active release when its managed systemd units need a repair, or "". */
  repair: string;
}

/**
 * What the Core card offers, from `update --status` or `--check` values.
 * A check's `update` key is authoritative; plain status compares the tags.
 */
export function releaseOffer(status: Record<string, string>): ReleaseOffer {
  const current = releaseTag(status.current);
  const latest = releaseTag(status.latest);
  const migrate = status.schema === "migration required" ? latest : "";
  const newer = status.update !== undefined ? status.update === "available" : !current ? !!latest : newerRelease(latest, current);
  const install = newer && !migrate ? latest : "";
  // `--version` with the active tag reinstalls its units (update-core-release.sh, repair_active_managed_units).
  const broken = status.update === "repair required" || status.systemd_units === "repair-required";
  return { current, previous: releaseTag(status.previous), install, migrate, repair: broken && !install && !migrate ? current : "" };
}

/** The Core card headline. */
export function releaseHeadline(offer: ReleaseOffer): string {
  if (offer.migrate) return `${offer.migrate} needs a schema migration`;
  if (offer.install) return `${offer.install} is ready to install`;
  if (offer.repair) return `${offer.repair} needs a unit repair`;
  return offer.current ? `${offer.current} is active` : "Release status";
}

/** The badge of a CLI runtime card; a failed check outranks an available update. */
export function runtimeBadge(installed: boolean, healthy: boolean, update: boolean): { tone: Tone; label: string } {
  if (!installed) return { tone: "idle", label: "Not installed" };
  if (!healthy) return { tone: "error", label: "Needs attention" };
  return update ? { tone: "warn", label: "Update available" } : { tone: "ok", label: "Installed" };
}

/** The terminal command for the card's offer: migrate, install or repair; "" when there is none. */
export function cliCommand(offer: ReleaseOffer): string {
  if (offer.migrate) return `sudo jarvis update --migrate ${offer.migrate}`;
  const version = offer.install || offer.repair;
  return version ? `sudo jarvis update --version ${version}` : "";
}

// What a Core install, rollback and migration do to running clients, from
// deploy/systemd/update-core-release.sh: readiness is retried for about a minute,
// after Laya reloads its models if it was running.
export const DOWNTIME = "Core restarts and connected clients disconnect until it is ready again, usually about a minute; longer if Laya reloads its models or the updater restores the previous release.";

/** One long operation. It lives outside the view, so a lock or a page change cannot lose its outcome. */
export interface TrackedOperation {
  /** "Installing Core v0.0.68". */
  doing: string;
  /** "Install Core v0.0.68". */
  done: string;
  /** Who owns the work while it runs. */
  note: string;
  startedAt: number;
  finishedAt: number | null;
  result: OperationResult | null;
  error: string;
}
export interface OperationStore { current: TrackedOperation | null }

export const isRunning = (store: OperationStore): boolean => !!store.current && store.current.finishedAt === null;

/**
 * Runs `work` and records it in `store`. One operation at a time: while one
 * runs, another resolves to null without starting. The record is never
 * cleared by a lock; the next operation replaces it.
 */
export async function track(
  store: OperationStore,
  labels: Pick<TrackedOperation, "doing" | "done" | "note">,
  work: () => Promise<OperationResult>,
  errorText: (error: unknown) => string,
  now: () => number = Date.now,
): Promise<OperationResult | null> {
  if (isRunning(store)) return null;
  store.current = { ...labels, startedAt: now(), finishedAt: null, result: null, error: "" };
  // Writes go through the store, so a reactive store re-renders.
  const op = store.current;
  try {
    op.result = await work();
  } catch (error) {
    op.error = errorText(error);
  }
  op.finishedAt = now();
  return op.result;
}

const hhmm = (at: number) => {
  const date = new Date(at);
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
};

export type OperationPhase = "running" | "done" | "failed";
export interface OperationView { phase: OperationPhase; badge: string; headline: string; detail: string }

/** The banner for a tracked operation: in progress, or the last outcome with its time. */
export function operationView(op: TrackedOperation): OperationView {
  if (op.finishedAt === null) {
    return { phase: "running", badge: "In progress", headline: op.doing, detail: `Started ${hhmm(op.startedAt)}. ${op.note}` };
  }
  const ok = !op.error && !!op.result?.success;
  return {
    phase: ok ? "done" : "failed",
    badge: ok ? "Done" : "Failed",
    headline: `Last operation: ${op.done}`,
    detail: `Finished ${hhmm(op.finishedAt)} (started ${hhmm(op.startedAt)}).`,
  };
}
