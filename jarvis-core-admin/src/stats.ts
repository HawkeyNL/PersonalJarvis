// Pure formatting and summary helpers for the dashboard and usage analysis.
// No Tauri imports, so `npm run test:unit` runs them under plain Node.

export const NOT_MEASURED = "Not measured yet";

const integerFormat = new Intl.NumberFormat("en");
const compactFormat = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
const eurFormat = new Intl.NumberFormat("en", { style: "currency", currency: "EUR" });

export const integer = (value: number) => integerFormat.format(value);
export const compact = (value: number) => compactFormat.format(value);
export const eur = (value: number) => eurFormat.format(value);
export const ms = (value: number) => (value >= 1000 ? `${(value / 1000).toFixed(1)} s` : `${integerFormat.format(value)} ms`);

/** null/undefined is "not measured" (older Core, or not instrumented yet). A
 *  measured zero is a real zero and is shown as one. */
export function measured(value: number | null | undefined, format: (value: number) => string = integer): string {
  return value === null || value === undefined ? NOT_MEASURED : format(value);
}

export type ServiceTone = "ok" | "warn" | "error" | "idle";

/** Services up out of total, and the tone the dashboard orb takes from them. */
export function serviceSummary(services: Record<string, string> | null | undefined): { up: number; total: number; tone: ServiceTone } {
  const states = Object.values(services ?? {}).map((state) => state.toLowerCase());
  const up = states.filter((state) => state === "active").length;
  const total = states.length;
  if (!total) return { up, total, tone: "idle" };
  if (up === total) return { up, total, tone: "ok" };
  return { up, total, tone: states.some((state) => state === "failed") ? "error" : "warn" };
}

/** Share of the budget spent, clamped to 0–100; 0 without a budget. */
export function budgetPercent(spent: number, budget: number): number {
  return budget > 0 ? Math.min(100, Math.max(0, (spent / budget) * 100)) : 0;
}
