import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { clean, findRepoRoot } from "../agentStream.mjs";

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
