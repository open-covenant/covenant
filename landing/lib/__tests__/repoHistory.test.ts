import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { LOOP_AUTHOR, readHistory } from "../repoHistory.mjs";

// readHistory feeds the header's commit total and the loop's latest commit. A
// deploy checkout is shallow, so it must answer from the remote's full history
// in that case, and from the checkout itself when the checkout is complete.

const tmps: string[] = [];
const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };

function makeTmp() {
  const dir = mkdtempSync(join(tmpdir(), "cov-history-"));
  tmps.push(dir);
  return dir;
}

const sh = (root: string, ...args: string[]) =>
  execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], { encoding: "utf8", env }).trim();

// The loop's commit is third of four, so it is not the tip.
function makeOrigin() {
  const root = makeTmp();
  sh(root, "init", "-q", "-b", "main");
  const commit = (msg: string, email: string) => {
    writeFileSync(join(root, "f.txt"), `${msg}\n`);
    sh(root, "add", "-A");
    sh(root, "-c", "user.name=tester", "-c", `user.email=${email}`, "commit", "-q", "-m", msg);
    return sh(root, "rev-parse", "HEAD").slice(0, 7);
  };
  commit("one", "person@example.test");
  commit("two", "person@example.test");
  const loop = commit("three", LOOP_AUTHOR);
  commit("four", "person@example.test");
  return { root, loop };
}

afterEach(() => {
  while (tmps.length) rmSync(tmps.pop()!, { recursive: true, force: true });
});

describe("readHistory", () => {
  it("reads a complete checkout directly", () => {
    const { root, loop } = makeOrigin();
    expect(readHistory({ repoRoot: root, url: "file:///nonexistent" })).toEqual({ commits: 4, head: loop });
  });

  it("reads the remote's full history when the checkout is shallow", () => {
    const { root, loop } = makeOrigin();
    const checkout = join(makeTmp(), "checkout");
    execFileSync("git", ["clone", "-q", "--depth", "1", `file://${root}`, checkout], { env });
    expect(sh(checkout, "rev-list", "--count", "HEAD")).toBe("1");

    expect(readHistory({ repoRoot: checkout, url: `file://${root}` })).toEqual({ commits: 4, head: loop });
  });

  it("returns nulls instead of throwing when the remote is unreachable", () => {
    const { root } = makeOrigin();
    const checkout = join(makeTmp(), "checkout");
    execFileSync("git", ["clone", "-q", "--depth", "1", `file://${root}`, checkout], { env });

    const result = readHistory({ repoRoot: checkout, url: "file:///nonexistent/repo.git" });
    expect(result).toMatchObject({ commits: null, head: null });
    expect(result.reason).toMatch(/does not appear to be a git repository|not found|No such/i);
  });
});
