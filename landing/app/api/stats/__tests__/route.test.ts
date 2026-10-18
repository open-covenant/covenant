import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LOOP_AUTHOR } from "@/lib/repoHistory.mjs";

// GET /api/stats is the public HUD source: it reads main's commit total (the
// Link header's last page) and the loop's latest commit from GitHub, plus the
// README test/crate metrics. When GitHub refuses, it falls back to its last
// reading, the deployed checkout's git and a build-time snapshot, never letting
// the count regress below what it already knows. The module memoizes the body
// for a TTL. These tests pin the GitHub-primary path, each fallback, the
// snapshot floor and the cache short-circuit. The module-level state is reset
// between arms by re-importing the route fresh.

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
  return { root, sh, commit, count: () => Number(sh("rev-list", "--count", "HEAD")) };
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

function ghRefused() {
  return { ok: false, status: 403, headers: new Headers(), json: async () => ({}) };
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
    // The header links the loop's own latest commit, so the second commit is
    // authored as the loop.
    const { root, sh, commit, count } = makeRepo();
    const head = commit("one");
    sh("config", "user.email", LOOP_AUTHOR);
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

  it("names the loop's latest commit from the snapshot when GitHub refuses", async () => {
    // No checkout here, as on a shallow deploy, so only the snapshot knows the sha.
    const dir = makeTmp();
    mkdirSync(join(dir, "public"), { recursive: true });
    writeFileSync(join(dir, "public", "agent-stream.json"), JSON.stringify({ totalCommits: 3741, head: "0e9b873" }));
    vi.spyOn(process, "cwd").mockReturnValue(dir);
    stubFetch((url) =>
      url.includes("api.github.com")
        ? ghRefused()
        : { ok: true, text: async () => README },
    );

    const GET = await freshGet();
    const body = await (await GET()).json();
    expect(body.head).toBe("0e9b873");
    expect(body.commits).toBe(3741);
  });

  it("never links the tip when only the loop's read is refused", async () => {
    // At the edge of the rate limit one of the two reads can fail alone. The
    // tip may be a human or merge commit, so the snapshot names the loop's.
    const dir = makeTmp();
    mkdirSync(join(dir, "public"), { recursive: true });
    writeFileSync(join(dir, "public", "agent-stream.json"), JSON.stringify({ totalCommits: 10, head: "0e9b873" }));
    vi.spyOn(process, "cwd").mockReturnValue(dir);
    stubFetch((url) => {
      if (!url.includes("api.github.com")) return { ok: true, text: async () => README };
      return url.includes("author=")
        ? ghRefused()
        : ghResponse(4321, "fffffff1234567");
    });

    const GET = await freshGet();
    const body = await (await GET()).json();
    expect(body.head).toBe("0e9b873");
    expect(body.commits).toBe(4321);
  });

  it("keeps the last GitHub reading through a later refusal", async () => {
    vi.spyOn(process, "cwd").mockReturnValue(makeTmp());
    let refuse = false;
    stubFetch((url) => {
      if (!url.includes("api.github.com")) return { ok: true, text: async () => README };
      return refuse
        ? ghRefused()
        : ghResponse(4321, "abcdef1234567");
    });
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000_000);

    const GET = await freshGet();
    const first = await (await GET()).json();
    expect(first.head).toBe("abcdef1");

    // Past the TTL, so the route asks GitHub again and is refused.
    refuse = true;
    now.mockReturnValue(1_000_000 + 10 * 60_000);
    const second = await (await GET()).json();
    expect(second.head).toBe("abcdef1");
    expect(second.commits).toBe(4321);
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
    // Two GET calls, but only the first fetched: githubHead's two reads (the
    // commit total and the loop's latest commit) plus githubMetrics' README.
    expect(f).toHaveBeenCalledTimes(3);
  });
});
