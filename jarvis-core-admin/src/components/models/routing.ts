// Staged routing changes and the confirms the Models page asks for. No Vue or
// Tauri imports, so `npm run test:unit` loads this file directly.
import type { ModelRecord, PaidApi, ResearchWebSearch, RouteEntry, RouteMutation, RouteTier, RoutingReport, TierRoute } from "../../admin";

export const TIERS: { tier: RouteTier; label: string }[] = [
  { tier: "cheap", label: "Cheap" },
  { tier: "default", label: "Default" },
  { tier: "hard", label: "Hard" },
];
// jarvis_llm::ROUTING_PROVIDERS. Only the subscriptions and ollama (local) are not metered.
export const ROUTE_PROVIDERS = ["anthropic-api", "openai-api", "deepseek-api", "xai-api", "zai-api", "ollama", "ollama-cloud", "huggingface", "claude-cli", "codex-cli"];
const SUBSCRIPTIONS = ["claude-cli", "codex-cli"];
export const MAX_CHAIN = 9;

export type Kind = "subscription" | "local" | "paid API";
/** Any provider that is not a subscription or local Ollama counts as paid (fail closed). */
export const kind = (provider: string): Kind => SUBSCRIPTIONS.includes(provider) ? "subscription" : provider === "ollama" ? "local" : "paid API";
export const entryKey = (entry: RouteEntry) => `${entry.provider}/${entry.model}`;
export function paidAfterSubscription(chain: RouteEntry[]): boolean {
  const first = chain.findIndex((entry) => kind(entry.provider) === "subscription");
  return first >= 0 && chain.slice(first + 1).some((entry) => kind(entry.provider) === "paid API");
}

/**
 * The owner's routing as edited on the page, before Apply. A tier of `null`
 * uses the built-in order. Lives in component memory only.
 */
export interface Staged { paid_api: PaidApi; research_web_search: ResearchWebSearch; tiers: Record<RouteTier, TierRoute | null> }
export type ChangeKey = "paid_api" | "research_web_search" | RouteTier;
export interface Change { key: ChangeKey; request: RouteMutation; summary: string; problem?: string }

const copyTier = (tier: TierRoute | null | undefined): TierRoute | null =>
  tier ? { chain: tier.chain.map((entry) => ({ ...entry })), metered_after_subscription: tier.metered_after_subscription } : null;

/** What Core has saved, as an editable copy. */
export function baseline(report: RoutingReport): Staged {
  const routing = report.routing;
  return {
    paid_api: routing?.paid_api ?? "allowed",
    research_web_search: routing?.research_web_search ?? "off",
    tiers: { cheap: copyTier(routing?.tiers.cheap), default: copyTier(routing?.tiers.default), hard: copyTier(routing?.tiers.hard) },
  };
}

function sameTier(saved: TierRoute | null, draft: TierRoute | null): boolean {
  // An order that was started and emptied again on a built-in tier changes nothing.
  if (!saved && (!draft || !draft.chain.length)) return true;
  return !!saved && !!draft && JSON.stringify([saved.chain, saved.metered_after_subscription]) === JSON.stringify([draft.chain, draft.metered_after_subscription]);
}
const chainText = (chain: RouteEntry[]) => chain.map(entryKey).join(" → ");

/**
 * The broker calls Apply makes, in page order: paid APIs, research web search,
 * then the Cheap, Default and Hard tiers. Each is the request the matching
 * button sent before staging. A change with a `problem` blocks Apply.
 */
export function changes(report: RoutingReport, staged: Staged): Change[] {
  const saved = baseline(report);
  const list: Change[] = [];
  if (staged.paid_api !== saved.paid_api) {
    list.push({ key: "paid_api", request: { action: "paid_api", state: staged.paid_api }, summary: staged.paid_api === "off" ? "Paid APIs: turn off" : "Paid APIs: allow" });
  }
  if (staged.research_web_search !== saved.research_web_search) {
    list.push({ key: "research_web_search", request: { action: "research_web_search", state: staged.research_web_search }, summary: `Research web search: turn ${staged.research_web_search}` });
  }
  for (const { tier, label } of TIERS) {
    const draft = staged.tiers[tier];
    if (sameTier(saved.tiers[tier], draft)) continue;
    if (!draft) {
      list.push({ key: tier, request: { action: "reset", tier }, summary: `${label}: use the built-in order (removes your order)` });
      continue;
    }
    const metered = draft.metered_after_subscription;
    const problem = !draft.chain.length ? `${label}: add a model or use the built-in order.`
      : !metered && paidAfterSubscription(draft.chain) ? `${label}: a paid API follows a subscription. Move it up, remove it, or allow it explicitly.`
      : undefined;
    list.push({
      key: tier,
      request: { action: "set", tier, chain: draft.chain.map((entry) => ({ ...entry })), metered_after_subscription: metered },
      summary: `${label}: ${chainText(draft.chain) || "empty order"}${metered ? " (paid API allowed after a subscription)" : ""}`,
      problem,
    });
  }
  return list;
}

/**
 * After a failed Apply: Core's new state, with the changes that were not
 * applied staged again so the owner can retry or discard them.
 */
export function restage(report: RoutingReport, staged: Staged, keep: ChangeKey[]): Staged {
  const next = baseline(report);
  for (const key of keep) {
    if (key === "paid_api") next.paid_api = staged.paid_api;
    else if (key === "research_web_search") next.research_web_search = staged.research_web_search;
    else next.tiers[key] = copyTier(staged.tiers[key]);
  }
  return next;
}

/**
 * Sends each change in order and stops at the first failure, or when the page
 * is gone (`alive` false: locked or navigated away). Returns how many were sent.
 */
export async function applyInOrder<T>(list: Change[], send: (request: RouteMutation) => Promise<T>, alive: () => boolean, done?: (result: T, index: number) => void): Promise<{ applied: number; error?: unknown }> {
  for (const [index, change] of list.entries()) {
    if (!alive()) return { applied: index };
    let result: T;
    try { result = await send(change.request); }
    catch (error) { return { applied: index, error }; }
    done?.(result, index);
  }
  return { applied: list.length };
}

export interface ConfirmSpec { title: string; detail: string; label: string; tone: "neutral" | "primary" | "danger" }

const PAID_DETAIL = "Metered provider APIs become eligible again in every tier, within the monthly budget. Providers bill each call.";
const RESEARCH_DETAIL = "Explicit Research requests send only the question, with a fixed instruction, to your Claude or ChatGPT subscription, which searches the web with its provider-hosted search. No memory, history or device data is sent. Web results are untrusted; answers list their sources.";
const STAGED_NOTE = "Nothing changes until you apply.";

/** Confirms before a change is staged; the Apply confirm follows later. */
export function stageConfirm(action: "allow_paid" | "research_on" | "reset", tierLabel = ""): ConfirmSpec {
  switch (action) {
    case "allow_paid": return { title: "Allow paid APIs?", detail: `${PAID_DETAIL} ${STAGED_NOTE}`, label: "Allow paid APIs", tone: "danger" };
    case "research_on": return { title: "Turn on research web search?", detail: `${RESEARCH_DETAIL} ${STAGED_NOTE}`, label: "Turn on web search", tone: "primary" };
    case "reset": return { title: `Use the built-in order for ${tierLabel}?`, detail: `Your order for the ${tierLabel} tier, including edits you have not applied, is replaced by the built-in order. ${STAGED_NOTE}`, label: `Use built-in order for ${tierLabel}`, tone: "danger" };
  }
}

/**
 * Apply lists every change. Danger when it can bill paid APIs or removes an
 * owner order; restrictive changes are primary.
 */
export function applyConfirm(list: Change[]): ConfirmSpec {
  const risky = list.some((change) => (change.request.action === "paid_api" && change.request.state === "allowed") || change.request.action === "reset" || (change.request.action === "set" && change.request.metered_after_subscription));
  const notes = [
    list.some((change) => change.request.action === "paid_api" && change.request.state === "allowed") ? PAID_DETAIL : "",
    list.some((change) => change.request.action === "research_web_search" && change.request.state === "on") ? RESEARCH_DETAIL : "",
  ].filter(Boolean);
  const count = list.length === 1 ? "1 routing change" : `${list.length} routing changes`;
  return {
    title: `Apply ${count}?`,
    detail: [
      `Applying restarts Jarvis Core${list.length > 1 ? " once per change" : ""}. Changes are applied in this order and stop at the first failure:`,
      list.map((change, index) => `${index + 1}. ${change.summary}`).join("\n"),
      ...notes,
    ].join("\n\n"),
    label: "Apply and restart Core",
    tone: risky ? "danger" : "primary",
  };
}

/** A model whose calls a provider bills: any provider but a subscription or local Ollama. */
export const isPaidModel = (row: ModelRecord) => kind(row.provider) === "paid API";

/** Enable and Disable in the catalog. Enabling a paid model is danger. */
export function modelConfirm(action: "enable" | "disable", row: ModelRecord): ConfirmSpec {
  const name = `${row.provider}/${row.model}`;
  if (action === "disable") return { title: "Disable this model?", detail: `${name} will no longer be eligible for routing.`, label: `Disable ${row.model}`, tone: "danger" };
  return isPaidModel(row)
    ? { title: "Enable this paid model?", detail: `${name} becomes eligible for routing. It is a paid API: the provider bills each call routing sends to it, within the monthly budget.`, label: `Enable ${row.model}`, tone: "danger" }
    : { title: "Enable this model?", detail: `${name} becomes eligible for routing.`, label: `Enable ${row.model}`, tone: "primary" };
}

/** Saving a Hugging Face inference route. */
export const routeConfirm = (row: ModelRecord, route: string): ConfirmSpec => ({
  title: "Save this route?",
  detail: `${row.model} will use the Hugging Face route "${route}". Route selection does not enable this model.`,
  label: `Save route ${route}`,
  tone: "primary",
});
