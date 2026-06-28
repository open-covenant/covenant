import { runGit } from "./git";

// A commit predates the witness loop when it is an ancestor of the configured
// cutover sha; its anchors then render historical-gray instead of being checked.
// With no cutover configured the gate is open — nothing predates, so anchors are
// checked for every commit. merge-base --is-ancestor signals via exit code, which
// runGit surfaces as "" (success -> ancestor -> predates) or null (non-zero -> not
// an ancestor); any git failure therefore reads as "does not predate", failing
// toward checking anchors rather than silently hiding them.
export function predatesCutover(repoRoot: string, sha: string, cutoverSha: string): boolean {
  if (!cutoverSha) return false;
  return runGit(repoRoot, ["merge-base", "--is-ancestor", sha, cutoverSha]) !== null;
}
