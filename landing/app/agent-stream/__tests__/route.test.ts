import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// GET /agent-stream feeds the landing terminal. It regenerates the feed from the
// runtime checkout's real git history when that history is genuinely rich, but
// prefers the committed public/agent-stream.json snapshot when there is no git
// root or the live history is thin (a shallow deploy clone) so the terminal never
// goes blank or shows a stub. The module memoizes the body for a TTL. These tests
// pin the rich-vs-thin decision boundary (>= 12 commits), the snapshot fallback,
// and the cache short-circuit, driven against throwaway repos and a fresh import
// per arm so the module-level cache does not bleed. The route's generateStream
// shells out to git, so the global/system git config is neutralized for
// determinism and the arms run shell-bound git, hence the wider timeout.

const SLOW = 20000;
const tmps: string[] = [];
const SNAPSHOT = JSON.stringify({ lines: [{ k: "commit", t: "snap" }], commits: 3181 });

async function freshGet() {
  vi.resetModules();
  return (await import("../route")).GET;
}

function makeTmp() {
  const dir = mkdtempSync(join(tmpdir(), "cov-stream-"));
  tmps.push(dir);
  return dir;
}

function withSnapshot(dir: string) {
  mkdirSync(join(dir, "public"), { recursive: true });
  writeFileSync(join(dir, "public", "agent-stream.json"), SNAPSHOT);
  return dir;
}

function initRepo() {
  const root = makeTmp();
  const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
  const sh = (...args: string[]) =>
    execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
      encoding: "utf8",
      env,
    });
  sh("init", "-q", "-b", "main");
  sh("config", "user.name", "covtester");
  sh("config", "user.email", "cov@example.test");
  return { root, sh };
}

function makeRepo(commits: number) {
  const { root, sh } = initRepo();
  for (let i = 0; i < commits; i += 1) {
    writeFileSync(join(root, "f.txt"), `line ${i}\nbody ${i}\n`);
    if (i === 0) sh("add", "-A");
    sh("commit", "-aqm", `commit ${i}`);
  }
  return root;
}

// Commits with no file changes: generateStream counts them (commits >= 12) but
// renders no lines, so only the `!data.lines.length` operand can trigger the
// snapshot fallback.
function makeEmptyRepo(commits: number) {
  const { root, sh } = initRepo();
  for (let i = 0; i < commits; i += 1) sh("commit", "--allow-empty", "-qm", `empty ${i}`);
  return root;
}

beforeEach(() => {
  // generateStream's git helper inherits process.env; neutralize ambient config
  // so the live-history decision is identical on every machine and in CI.
  vi.stubEnv("GIT_CONFIG_GLOBAL", "/dev/null");
  vi.stubEnv("GIT_CONFIG_SYSTEM", "/dev/null");
});

afterEach(() => {
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
  while (tmps.length) rmSync(tmps.pop()!, { recursive: true, force: true });
});

describe("agent-stream route GET", () => {
  it(
    "regenerates the feed from rich live git history",
    async () => {
      vi.spyOn(process, "cwd").mockReturnValue(makeRepo(12));
      const body = await (await (await freshGet())()).json();
      expect(body.commits).toBe(12);
      expect(body.lines.length).toBeGreaterThan(0);
    },
    SLOW,
  );

  it(
    "serves the committed snapshot when the live history is too thin",
    async () => {
      // 11 commits is below the >= 12 richness floor, so the route must fall back.
      vi.spyOn(process, "cwd").mockReturnValue(withSnapshot(makeRepo(11)));
      const body = await (await (await freshGet())()).json();
      expect(body.commits).toBe(3181); // the snapshot, not the 11-commit live feed
      expect(body).toEqual(JSON.parse(SNAPSHOT));
    },
    SLOW,
  );

  it(
    "serves the committed snapshot when the history has commits but renders no lines",
    async () => {
      vi.spyOn(process, "cwd").mockReturnValue(withSnapshot(makeEmptyRepo(12)));
      const body = await (await (await freshGet())()).json();
      expect(body).toEqual(JSON.parse(SNAPSHOT));
    },
    SLOW,
  );

  it(
    "serves the committed snapshot when there is no git root at all",
    async () => {
      vi.spyOn(process, "cwd").mockReturnValue(withSnapshot(makeTmp()));
      const body = await (await (await freshGet())()).json();
      expect(body).toEqual(JSON.parse(SNAPSHOT));
    },
    SLOW,
  );

  it(
    "serves the memoized body within the TTL even as the source changes",
    async () => {
      const cwd = vi.spyOn(process, "cwd").mockReturnValue(makeRepo(12));
      const GET = await freshGet();
      const first = await (await GET()).json();
      expect(first.commits).toBe(12);

      // Point cwd at a bare snapshot dir that would otherwise serve the snapshot;
      // the cached live body must still win within the TTL.
      cwd.mockReturnValue(withSnapshot(makeTmp()));
      const second = await (await GET()).json();
      expect(second).toEqual(first);
      expect(second.commits).toBe(12);
    },
    SLOW,
  );
});
