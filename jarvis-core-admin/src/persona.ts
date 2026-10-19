// Pure persona display helpers. No Tauri imports, so `npm run test:unit`
// runs them under plain Node. The privileged side decides; these only explain.
import type { PersonaStatus } from "./admin";

const problems: Record<string, string> = {
  empty: "Empty",
  too_large: "Larger than the size limit",
  not_utf8: "Not valid UTF-8",
  control_character: "Contains a control or invisible formatting character",
  missing_heading: "Does not start with a \"# \" heading",
  secret_like: "Appears to contain a key or token",
  hash_mismatch: "Content does not match its hash",
};

const sourceProblems: Record<string, string> = {
  configuration_unavailable: "The private agent configuration is unavailable.",
  configuration_invalid: "The private agent configuration is invalid or unsafe.",
  checkout_unavailable: "The private checkout is unavailable.",
  checkout_not_root_controlled: "The private checkout is not root-controlled.",
  checkout_not_git: "The private checkout is not a Git checkout.",
  no_commit: "The private checkout has no valid commit.",
  no_persona: "The private checkout's commit has no persona.",
};

export const shortHash = (hash: string) => hash.slice(0, 12);

export function problemLabel(code: string | null): string {
  return code === null ? "Valid" : problems[code] ?? "Refused by validation";
}

export function sourceProblemLabel(code: string | null): string {
  return (code && sourceProblems[code]) ?? "The candidate is unavailable; run `sudo jarvis persona status` for details.";
}

/** Why the candidate cannot be installed, or null when it can. */
export function installBlocker(status: PersonaStatus): string | null {
  const candidate = status.candidate;
  if (!candidate) return sourceProblemLabel(status.candidate_error);
  if (candidate.signature !== "verified") return "The candidate commit is not signed by an allowed signer.";
  if (!candidate.sha256) return "The candidate is larger than the size limit.";
  if (candidate.problem !== null) return `The candidate is refused: ${problemLabel(candidate.problem)}.`;
  if (candidate.sha256 === status.installed?.sha256) return "The candidate is already installed.";
  return null;
}
