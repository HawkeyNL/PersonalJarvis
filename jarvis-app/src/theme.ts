// Accent colour theming. Green is the Jarvis default; the user can switch the
// accent from Settings. The choice is persisted and re-applied on launch.
export type Accent = "green" | "cyan" | "amber" | "violet";

export const PRESETS: Record<Accent, [string, string]> = {
  green: ["#34f5a0", "#7dffc0"],
  cyan: ["#22c8db", "#7be7f2"],
  amber: ["#f0a848", "#ffcf8a"],
  violet: ["#9b7dff", "#c9b8ff"],
};

export const ACCENTS = Object.keys(PRESETS) as Accent[];

const KEY = "jarvis.accent";

export function currentAccent(): Accent {
  const v = localStorage.getItem(KEY) as Accent | null;
  return v && v in PRESETS ? v : "green";
}

export function applyAccent(a: Accent): void {
  const [c1, c2] = PRESETS[a] ?? PRESETS.green;
  const root = document.documentElement;
  root.style.setProperty("--accent", c1);
  root.style.setProperty("--accent-2", c2);
  localStorage.setItem(KEY, a);
}

/** Apply the saved accent on app start. */
export function initAccent(): void {
  applyAccent(currentAccent());
}

/** Parse the live `--accent-2` value into [r,g,b] for canvas drawing. */
export function accentRgb(): [number, number, number] {
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue("--accent-2")
    .trim();
  const m = /^#?([0-9a-f]{6})$/i.exec(raw);
  if (!m) return [125, 255, 192];
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}
