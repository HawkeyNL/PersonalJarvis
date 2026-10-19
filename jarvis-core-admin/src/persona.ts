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

export const shortHash = (hash: string) => hash.slice(0, 12);

export function problemLabel(code: string | null): string {
  return code === null ? "Valid" : problems[code] ?? "Refused by validation";
}

/** Why the candidate cannot be installed, or null when it can. */
export function installBlocker(status: PersonaStatus): string | null {
  const candidate = status.candidate;
  if (!candidate) return status.candidate_error ?? "No candidate is available.";
  if (candidate.signature !== "verified") return "The candidate commit is not signed by an allowed signer.";
  if (!candidate.sha256) return "The candidate is larger than the size limit.";
  if (candidate.problem !== null) return `The candidate is refused: ${problemLabel(candidate.problem)}.`;
  if (candidate.sha256 === status.installed?.sha256) return "The candidate is already installed.";
  return null;
}
