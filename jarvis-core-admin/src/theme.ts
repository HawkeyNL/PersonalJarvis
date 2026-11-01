// Light / Dark / System theme, as in the Jarvis client. The choice is
// persisted; "system" (the default) follows prefers-color-scheme and updates
// live when the OS switches. The resolved theme lands on
// <html data-theme="light|dark">, see tokens.css.
export type ThemeChoice = "light" | "dark" | "system";
export type Theme = "light" | "dark";

export const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
];

const KEY = "jarvis.theme";
const DARK_QUERY = "(prefers-color-scheme: dark)";

type Env = Pick<Window, "localStorage" | "matchMedia" | "document" | "requestAnimationFrame">;

/** The saved choice; "system" when nothing valid is stored or storage is unavailable. */
export function currentTheme(env: Env = window): ThemeChoice {
  let saved: string | null = null;
  try {
    saved = env.localStorage.getItem(KEY);
  } catch {
    // Storage can be blocked or missing; fall back to the OS preference.
  }
  return saved === "light" || saved === "dark" ? saved : "system";
}

export function resolveTheme(choice: ThemeChoice, prefersDark: boolean): Theme {
  return choice === "system" ? (prefersDark ? "dark" : "light") : choice;
}

function paint(env: Env, choice: ThemeChoice): void {
  const root = env.document.documentElement;
  const next = resolveTheme(choice, env.matchMedia(DARK_QUERY).matches);
  if (root.dataset.theme === next) return;
  // Swap without every colour transition firing at once.
  root.classList.add("no-tx");
  root.dataset.theme = next;
  void root.offsetHeight; // commit the new colours while transitions are off
  env.requestAnimationFrame(() => env.requestAnimationFrame(() => root.classList.remove("no-tx")));
}

// Kept in memory too, so the choice still applies for this run when storage fails.
let chosen: ThemeChoice = "system";

export function setTheme(choice: ThemeChoice, env: Env = window): void {
  chosen = choice;
  try {
    env.localStorage.setItem(KEY, choice);
  } catch {
    // Not persisted; the in-memory choice still applies until the app closes.
  }
  paint(env, choice);
}

/** Apply the saved theme on app start and follow OS changes while on "system". */
export function initTheme(env: Env = window): void {
  chosen = currentTheme(env);
  paint(env, chosen);
  env.matchMedia(DARK_QUERY).addEventListener("change", () => paint(env, chosen));
}
