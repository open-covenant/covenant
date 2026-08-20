import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

// GET /api/stats is the public HUD source: it tracks origin/main's head sha and
// commit total straight from GitHub (per_page=1 for the head, the Link header's
// last page for the total) plus the README test/crate metrics, and falls back to
// the deployed checkout's git and a build-time snapshot when GitHub is
// unreachable — never letting the count regress below what it already knows. The
// module memoizes the body for a TTL. These tests pin the GitHub-primary path,
// the git+README fallback, the snapshot floor, and the cache short-circuit. The
// module-level cache is reset between arms by re-importing the route fresh.

const tmps: string[] = [];

async function freshGet() {
  vi.resetModules();
  return (await import("../route")).GET;
}

function makeTmp() {
  const dir = mkdtempSync(join(tmpdir(), "cov-stats-"));
  tmps.push(dir);
  return dir;
}

function makeRepo() {
  const root = makeTmp();
  const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
  const sh = (...args: string[]) =>
    execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
      encoding: "utf8",
      env,
    }).trim();
  sh("init", "-q", "-b", "main");
  sh("config", "user.name", "covtester");
  sh("config", "user.email", "cov@example.test");
  const commit = (msg: string) => {
    writeFileSync(join(root, "f.txt"), `${msg}\n`);
    sh("add", "-A");
    sh("commit", "-q", "-m", msg);
    return sh("rev-parse", "--short", "HEAD");
  };
  return { root, commit, count: () => Number(sh("rev-list", "--count", "HEAD")) };
}

function ghResponse(linkPage: number, sha: string) {
  return {
    ok: true,
    headers: new Headers({
      link: `<https://api.github.com/repositories/1/commits?sha=main&per_page=1&page=${linkPage}>; rel="last"`,
    }),
    json: async () => [{ sha }],
  };
}

const README = "28 Rust crates, ~237k lines, 3197 source-discovered Rust tests including 464 live boundary tests.";

function stubFetch(impl: (url: string) => unknown) {
  const f = vi.fn(async (url: string) => impl(url));
  vi.stubGlobal("fetch", f);
  return f;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  while (tmps.length) rmSync(tmps.pop()!, { recursive: true, force: true });
});

describe("stats route GET", () => {
  it("tracks GitHub head, Link-derived commit total, and README metrics", async () => {
    // An empty (non-repo) cwd: no local git root and no snapshot file, so the
    // GitHub values pass through untouched.
    vi.spyOn(process, "cwd").mockReturnValue(makeTmp());
    stubFetch((url) =>
      url.includes("api.github.com")
        ? ghResponse(4321, "abcdef1234567")
        : { ok: true, text: async () => README },
    );

    const GET = await freshGet();
    const res = await GET();
    expect(res.status).toBe(200);
    const body = await res.json();

    expect(body.head).toBe("abcdef1"); // sha sliced to 7
    expect(body.commits).toBe(4321); // Link header's last page
    expect(body.tests).toBe("3197");
    expect(body.live).toBe("464");
    expect(body.crates).toBe("28");
    expect(typeof body.alphaSince).toBe("string");
  });

  it("falls back to the local checkout's git and README when GitHub is unreachable", async () => {
    const { root, commit, count } = makeRepo();
    const head = commit("one");
    const head2 = commit("two");
    writeFileSync(join(root, "README.md"), "12 Rust crates, 77 source-discovered Rust tests including 9 live boundary tests.");
    vi.spyOn(process, "cwd").mockReturnValue(root);
    const f = stubFetch(() => {
      throw new Error("network down");
    });

    const GET = await freshGet();
    const body = await (await GET()).json();

    expect(f).toHaveBeenCalled(); // GitHub was attempted first
    expect(body.head).toBe(head2);
    expect(head2).not.toBe(head);
    expect(body.commits).toBe(count()); // 2
    expect(body.tests).toBe("77");
    expect(body.live).toBe("9");
    expect(body.crates).toBe("12");
  });

  it("never lets the count regress below the build-time snapshot floor", async () => {
    const dir = makeTmp();
    mkdirSync(join(dir, "public"), { recursive: true });
    writeFileSync(join(dir, "public", "agent-stream.json"), JSON.stringify({ totalCommits: 99999 }));
    vi.spyOn(process, "cwd").mockReturnValue(dir);
    stubFetch((url) =>
      url.includes("api.github.com") ? ghResponse(10, "0000000aaaa") : { ok: true, text: async () => README },
    );

    const GET = await freshGet();
    const body = await (await GET()).json();
    // GitHub reports 10, the snapshot pins 99999 — the floor wins.
    expect(body.commits).toBe(99999);
  });

  it("serves the memoized body within the TTL without re-fetching", async () => {
    vi.spyOn(process, "cwd").mockReturnValue(makeTmp());
    const f = stubFetch((url) =>
      url.includes("api.github.com") ? ghResponse(7, "deadbeef999") : { ok: true, text: async () => README },
    );

    const GET = await freshGet();
    const first = await (await GET()).json();
    const second = await (await GET()).json();

    expect(second).toEqual(first);
    // Two GET calls, but githubHead+githubMetrics fired exactly once (first call).
    expect(f).toHaveBeenCalledTimes(2);
  });
});
