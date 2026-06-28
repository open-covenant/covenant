import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { clean, findRepoRoot, generateStream } from "../agentStream.mjs";

// clean() is the identity-protection scrubber the public witness surface relies
// on (app/api/verify redactAuthor, app/agent-stream): it nulls out a line that
// carries any runtime-derived identity token and rewrites /Users/<name> home
// paths to ~. Its leak tokens are derived at import time, so the token-drop arm
// is exercised with a stubbed env and a fresh module.
describe("clean", () => {
  it("returns null for nullish input", () => {
    expect(clean(null)).toBeNull();
    expect(clean(undefined)).toBeNull();
  });

  it("rewrites a home path to ~", () => {
    expect(clean("see /Users/alice/secret.txt")).toBe("see ~/secret.txt");
  });

  it("rewrites every home path, not just the first", () => {
    expect(clean("/Users/a/x and /Users/b/y")).toBe("~/x and ~/y");
  });

  it("strips a single trailing carriage return", () => {
    expect(clean("a commit subject\r")).toBe("a commit subject");
  });

  it("passes a clean line through unchanged", () => {
    expect(clean("a routine parser refactor")).toBe("a routine parser refactor");
  });
});

describe("clean leak-token drop", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  it("drops a line containing a runtime identity token", async () => {
    vi.stubEnv("USER", "covsentineluser");
    vi.resetModules();
    const { clean: freshClean } = await import("../agentStream.mjs");
    expect(freshClean("authored by covsentineluser")).toBeNull();
    expect(freshClean("an ordinary commit subject")).toBe("an ordinary commit subject");
  });
});

// findRepoRoot walks up from a start dir looking for .git (max 8 levels) and
// underpins repo resolution for the witness route (app/api/verify) and the live
// feed tail. Exercised against a controlled temp tree so the 8-level cap can be
// pinned without escaping into real ancestors.
describe("findRepoRoot", () => {
  let root: string;
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-reporoot-"));
    mkdirSync(join(root, ".git"));
    mkdirSync(join(root, "a", "b"), { recursive: true });
    // Eight levels below root, so root/.git sits one step past the 8-level cap.
    mkdirSync(join(root, "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"), { recursive: true });
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("returns the start dir when it holds .git", () => {
    expect(findRepoRoot(root)).toBe(resolve(root));
  });

  it("walks up to the nearest ancestor holding .git", () => {
    expect(findRepoRoot(join(root, "a", "b"))).toBe(resolve(root));
  });

  it("stops after eight levels and returns null past the cap", () => {
    const deep = join(root, "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8");
    expect(findRepoRoot(deep)).toBeNull();
  });
});

// generateStream renders real git history into the public agent-stream feed:
// commits oldest-first, capped file/body counts, diff lines classified as
// add/del/ctx, binary and skip-listed paths omitted, and every emitted line run
// through clean() so identity tokens and home paths never reach the witness
// surface. Driven against throwaway repos so the structured output is asserted
// without depending on this checkout's history. Local diff.renames keeps rename
// detection independent of ambient global git config; hooks are disabled so the
// project's commit guard does not run inside the fixtures.
describe("generateStream", () => {
  const repos: string[] = [];

  function makeRepo() {
    const root = mkdtempSync(join(tmpdir(), "cov-gen-"));
    repos.push(root);
    const env = {
      ...process.env,
      GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_CONFIG_SYSTEM: "/dev/null",
      GIT_AUTHOR_DATE: "2026-01-01T00:00:00",
      GIT_COMMITTER_DATE: "2026-01-01T00:00:00",
    };
    delete env.COVENANT_SESSION_ID;
    const sh = (...args: string[]) =>
      execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
        encoding: "utf8",
        env,
      });
    sh("init", "-q", "-b", "main");
    sh("config", "user.name", "covtester");
    sh("config", "user.email", "cov@example.test");
    sh("config", "diff.renames", "true");
    const write = (rel: string, body: string) => writeFileSync(join(root, rel), body);
    const commit = (msg: string) => {
      sh("add", "-A");
      sh("commit", "-q", "-m", msg);
    };
    return { root, write, commit };
  }

  afterEach(() => {
    while (repos.length) rmSync(repos.pop()!, { recursive: true, force: true });
  });

  it("streams commits oldest-first with subject, write, and commit lines", () => {
    const r = makeRepo();
    r.write("alpha.txt", "one\ntwo\n");
    r.commit("add alpha");
    r.write("alpha.txt", "one\ntwo\nthree\n");
    r.commit("extend alpha");

    const { commits, lines } = generateStream({ repoRoot: r.root });
    expect(commits).toBe(2);
    const cmds = lines.filter((l) => l.k === "cmd").map((l) => l.t);
    expect(cmds[0]).toMatch(/^git show [0-9a-f]+ {2}# add alpha$/);
    expect(cmds[1]).toMatch(/^git show [0-9a-f]+ {2}# extend alpha$/);
    const commitLines = lines.filter((l) => l.k === "commit").map((l) => l.t);
    expect(commitLines).toHaveLength(2);
    expect(commitLines[0]).toMatch(/^commited [0-9a-f]+$/);
  });

  it("redacts a home path in the commit subject", () => {
    const r = makeRepo();
    r.write("notes.txt", "x\n");
    r.commit("touch /Users/secret/notes");

    const cmd = generateStream({ repoRoot: r.root }).lines.find((l) => l.k === "cmd")!.t;
    expect(cmd).toContain("touch ~/notes");
    expect(cmd).not.toContain("/Users/");
  });

  it("labels an added file with its status and +add/-del sign", () => {
    const r = makeRepo();
    r.write("alpha.txt", "one\ntwo\nthree\n");
    r.commit("add alpha");

    const write = generateStream({ repoRoot: r.root }).lines.find((l) => l.k === "write")!.t;
    expect(write).toMatch(/^Writing alpha\.txt from [0-9a-f]+ \(added, \+3\/-0\)$/);
  });

  it("honors maxCommits, showing only the most recent commits", () => {
    const r = makeRepo();
    r.write("alpha.txt", "one\n");
    r.commit("add alpha");
    r.write("alpha.txt", "one\ntwo\n");
    r.commit("extend alpha");

    const { commits, lines } = generateStream({ repoRoot: r.root, maxCommits: 1 });
    expect(commits).toBe(1);
    const cmds = lines.filter((l) => l.k === "cmd");
    expect(cmds).toHaveLength(1);
    expect(cmds[0].t).toMatch(/# extend alpha$/);
  });

  it("normalizes a hunk header and classifies add, del, and context lines", () => {
    const r = makeRepo();
    r.write("f.txt", "one\ntwo\nthree\n");
    r.commit("init f");
    r.write("f.txt", "one\nTWO\nthree\nfour\n");
    r.commit("edit f");

    const { lines } = generateStream({ repoRoot: r.root, maxCommits: 1 });
    expect(lines.find((l) => l.k === "hunk")!.t).toBe("@@ -1,3 +1,4 @@");
    expect(lines.filter((l) => l.k === "add" || l.k === "del" || l.k === "ctx")).toEqual([
      { k: "ctx", t: " one" },
      { k: "del", t: "-two" },
      { k: "add", t: "+TWO" },
      { k: "ctx", t: " three" },
      { k: "add", t: "+four" },
    ]);
  });

  it("truncates a file body at maxBodyLines", () => {
    const r = makeRepo();
    r.write("big.txt", "l1\nl2\nl3\nl4\nl5\n");
    r.commit("add big");

    const { lines } = generateStream({ repoRoot: r.root, maxBodyLines: 2 });
    expect(lines.some((l) => l.k === "ctx" && l.t === " … (truncated)")).toBe(true);
    const adds = lines.filter((l) => l.k === "add");
    expect(adds).toHaveLength(1);
    expect(adds[0].t).toBe("+l1");
  });

  it("slices files at maxFilesPerCommit", () => {
    const r = makeRepo();
    r.write("a.txt", "a\n");
    r.write("b.txt", "b\n");
    r.write("c.txt", "c\n");
    r.commit("seed three");

    const writes = generateStream({ repoRoot: r.root, maxFilesPerCommit: 2 }).lines.filter(
      (l) => l.k === "write",
    );
    expect(writes).toHaveLength(2);
    expect(writes.map((w) => w.t.match(/Writing (\S+)/)![1])).toEqual(["a.txt", "b.txt"]);
  });

  it("detects deleted and renamed file statuses", () => {
    const r = makeRepo();
    r.write("a.txt", "a\n");
    r.write("b.txt", "keep\n");
    r.commit("seed");
    rmSync(join(r.root, "a.txt"));
    renameSync(join(r.root, "b.txt"), join(r.root, "b2.txt"));
    r.commit("delete a, rename b");

    const writes = generateStream({ repoRoot: r.root, maxCommits: 1 }).lines
      .filter((l) => l.k === "write")
      .map((l) => l.t);
    expect(writes.some((t) => /Writing a\.txt from [0-9a-f]+ \(deleted, \+0\/-1\)$/.test(t))).toBe(
      true,
    );
    expect(writes.some((t) => /Writing b2\.txt from [0-9a-f]+ \(renamed,/.test(t))).toBe(true);
  });

  it("omits the diff for a skip-listed path while still counting its lines", () => {
    const r = makeRepo();
    r.write("vec.svg", "<svg>a</svg>\n<g>b</g>\n");
    r.commit("add svg");

    const { lines } = generateStream({ repoRoot: r.root });
    expect(lines.find((l) => l.k === "meta")!.t).toBe("# added +2/-0 (diff omitted)");
    expect(lines.some((l) => l.k === "add")).toBe(false);
  });

  it("omits the diff for a binary file", () => {
    const r = makeRepo();
    writeFileSync(join(r.root, "blob.bin"), Buffer.from([0, 1, 2, 0, 255, 10, 0, 3]));
    r.commit("add binary");

    const { lines } = generateStream({ repoRoot: r.root });
    expect(lines.find((l) => l.k === "meta")!.t).toBe("# added +0/-0 (diff omitted)");
    expect(lines.some((l) => l.k === "add")).toBe(false);
  });

  it("returns no commits when no repository root resolves", () => {
    const nonGit = mkdtempSync(join(tmpdir(), "cov-norepo-"));
    const spy = vi.spyOn(process, "cwd").mockReturnValue(nonGit);
    try {
      expect(generateStream()).toEqual({ commits: 0, lines: [] });
    } finally {
      spy.mockRestore();
      rmSync(nonGit, { recursive: true, force: true });
    }
  });
});
