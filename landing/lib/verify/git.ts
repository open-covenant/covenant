import { execFileSync } from "node:child_process";

// Thin git runner for the witness verify surface (app/api/verify/[sha]): resolves
// commit metadata and ancestry, scoped to repoRoot via -C. Returns trimmed stdout,
// or null when git is unavailable or the command exits non-zero — a shallow deploy
// may carry only HEAD, so callers treat null as "git could not answer" and fall
// back to committed witness artifacts rather than failing the request.
export function runGit(repoRoot: string, args: string[]): string | null {
  try {
    return execFileSync("git", ["-C", repoRoot, ...args], {
      encoding: "utf8",
      maxBuffer: 4 * 1024 * 1024,
    }).trim();
  } catch {
    return null;
  }
}
