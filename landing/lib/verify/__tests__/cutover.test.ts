import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { predatesCutover } from "../cutover";

// predatesCutover gates the witness verify surface (app/api/verify/[sha]): a commit
// that is an ancestor of the configured cutover sha renders historical-gray instead
// of having its anchors checked. With no cutover configured the gate is open and
// nothing predates. Run against a throwaway two-commit repo so the merge-base
// --is-ancestor exit-code interpretation and the argument order are pinned — a flip
// would either hide in-loop anchors or expose pre-loop commits as checkable.
describe("predatesCutover", () => {
  let root: string;
  let parent: string;
  let child: string;

  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-cutover-"));
    const env = { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" };
    const sh = (...args: string[]) =>
      execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
        encoding: "utf8",
        env,
      }).trim();
    sh("init", "-q", "-b", "main");
    sh("config", "user.name", "covtester");
    sh("config", "user.email", "cov@example.test");
    writeFileSync(join(root, "f.txt"), "one\n");
    sh("add", "-A");
    sh("commit", "-q", "-m", "first");
    parent = sh("rev-parse", "HEAD");
    writeFileSync(join(root, "f.txt"), "one\ntwo\n");
    sh("add", "-A");
    sh("commit", "-q", "-m", "second");
    child = sh("rev-parse", "HEAD");
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("returns false when no cutover sha is configured", () => {
    // Dominant production path (WITNESS_CUTOVER_SHA empty until the pipeline ships):
    // the gate is open so anchors are checked for every commit. Also kills an
    // always-true regression.
    expect(predatesCutover(root, child, "")).toBe(false);
  });

  it("treats an ancestor of the cutover as predating the loop", () => {
    expect(predatesCutover(root, parent, child)).toBe(true);
  });

  it("treats a descendant of the cutover as not predating the loop", () => {
    // Paired with the ancestor case above, the asymmetry pins both the
    // !== null interpretation and the ancestor/descendant argument order.
    expect(predatesCutover(root, child, parent)).toBe(false);
  });

  it("treats the cutover commit itself as predating (inclusive ancestry)", () => {
    expect(predatesCutover(root, child, child)).toBe(true);
  });
});
