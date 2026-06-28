import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { runGit } from "../git";

// runGit is the witness verify surface's only git boundary (app/api/verify/[sha]):
// it resolves commit metadata and ancestry scoped to a repo via -C, trims stdout,
// and fails closed to null when git is unavailable or exits non-zero so a shallow
// deploy still renders committed witness artifacts. Driven against throwaway repos
// so the trim, the -C scoping, and the null fallback are pinned without depending
// on this checkout's history; hooks are disabled and global git config is ignored
// so the project's commit guard does not run inside the fixtures.
describe("runGit", () => {
  const repos: string[] = [];

  function makeRepo(seed: string) {
    const root = mkdtempSync(join(tmpdir(), "cov-rungit-"));
    repos.push(root);
    const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
    const sh = (...args: string[]) =>
      execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
        encoding: "utf8",
        env,
      }).trim();
    sh("init", "-q", "-b", "main");
    sh("config", "user.name", "covtester");
    sh("config", "user.email", "cov@example.test");
    writeFileSync(join(root, "f.txt"), seed);
    sh("add", "-A");
    sh("commit", "-q", "-m", seed);
    return { root, head: sh("rev-parse", "HEAD") };
  }

  afterEach(() => {
    while (repos.length) rmSync(repos.pop()!, { recursive: true, force: true });
  });

  it("returns trimmed stdout for a successful command", () => {
    const { root, head } = makeRepo("alpha");
    expect(head).toMatch(/^[0-9a-f]{40}$/);
    // Equality against the bare 40-char sha pins the .trim(): a dropped trim
    // returns "<sha>\n" and fails this assertion.
    expect(runGit(root, ["rev-parse", "HEAD"])).toBe(head);
  });

  it("returns null when the git command exits non-zero", () => {
    const { root } = makeRepo("alpha");
    expect(runGit(root, ["rev-parse", "--verify", "refs/heads/does-not-exist"])).toBeNull();
  });

  it("scopes execution to repoRoot via -C", () => {
    const a = makeRepo("alpha");
    const b = makeRepo("beta");
    // Distinct repos resolve distinct heads, so a dropped or constant -C arg
    // (which would resolve the process cwd instead) cannot pass both assertions.
    expect(runGit(a.root, ["rev-parse", "HEAD"])).toBe(a.head);
    expect(runGit(b.root, ["rev-parse", "HEAD"])).toBe(b.head);
    expect(a.head).not.toBe(b.head);
  });
});
