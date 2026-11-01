// Status tones and human labels for raw CLI keys and states, in one place.
// No Vue or Tauri imports, so `npm run test:unit` loads this file directly.

/** Status colour; the text label next to it always carries the meaning. */
export type Tone = "ok" | "idle" | "warn" | "error";

const OK = ["active", "enabled", "passed", "healthy", "configured", "known", "local", "yes", "available", "connected", "automatic", "up to date"];
// Intended or not-yet-relevant states: a mostly disabled catalog is normal, and
// a manual updater is the owner's choice, not a fault.
const NEUTRAL = ["disabled", "manual", "off", "not run this session", "logged_out"];
const ERROR = ["failed", "error", "inactive", "unavailable", "unhealthy"];

export function toneOf(state: string): Tone {
  const value = state.toLowerCase();
  if (OK.includes(value)) return "ok";
  if (NEUTRAL.includes(value)) return "idle";
  if (ERROR.some((word) => value.includes(word))) return "error";
  return "warn";
}

// Some keys report a value whose meaning depends on the key: the updater
// timer's "disabled" is manual updating, an update check's "not available" is
// up to date. These map to a state that toneOf and stateLabel understand.
const KEYED: Record<string, Record<string, string>> = {
  updater: { enabled: "automatic", disabled: "manual" },
  update: { available: "update available", "not available": "up to date" },
};
export const keyedState = (key: string, value: string): string => KEYED[key.toLowerCase()]?.[value] ?? value;

const FIELDS: Record<string, string> = {
  current: "Installed release",
  previous: "Previous release",
  latest: "Latest release",
  core_current: "Core version",
  core_latest: "Latest Core",
  cli_current: "CLI version",
  cli_latest: "Latest CLI",
  core_app_current: "Admin app version",
  core_app_latest: "Latest admin app",
  schema: "Database schema",
  systemd_units: "Service units",
  updater: "Updates",
  update: "Update",
};
/** Label for a raw CLI key such as `core_app_latest`. */
export const fieldLabel = (key: string): string => FIELDS[key] ?? sentence(key.replace(/_/g, " "));

const STATES: Record<string, string> = {
  logged_out: "Signed out",
  runtime_missing: "Runtime not installed",
  wrong_auth_mode: "Wrong sign-in method",
  incompatible_runtime: "Unsupported runtime version",
  host_unsupported: "Host tools failed checks",
  socket_ready: "Ready, starts on demand",
  overage_unverified: "Extra usage setting not verified",
  unverified: "Not verified",
  "not run this session": "Not run yet",
  "release-matched": "Matches release",
  "repair-required": "Repair required",
  "legacy-release": "Legacy release",
};
/** Label for a raw CLI state. Words are capitalised; versions and ids pass through. */
export const stateLabel = (state: string): string => STATES[state] ?? sentence(state.replace(/_/g, " "));

const sentence = (text: string) => (/^[a-z][a-z -]*$/.test(text) ? text[0].toUpperCase() + text.slice(1) : text);
