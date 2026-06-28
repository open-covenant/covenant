import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clean, commitEvent, parseTransitionLine } from "../agentBus.mjs";

const line = (o: Record<string, unknown>) => JSON.stringify(o);

// parseTransitionLine feeds the public live agent feed: it must fail closed on a
// malformed or incomplete transition line and redaction-clean the taskId/note it
// forwards. Identity-token redaction itself is covered in agentStream.test.ts;
// here we pin the parser arms and the field wiring with token-free vectors.
describe("parseTransitionLine", () => {
  it("returns null for malformed JSON", () => {
    expect(parseTransitionLine("{not json")).toBeNull();
  });

  it("returns null for a non-object payload", () => {
    expect(parseTransitionLine("null")).toBeNull();
  });

  it("returns null when taskId is missing", () => {
    expect(parseTransitionLine(line({ to: "ready" }))).toBeNull();
  });

  it("returns null when to is missing", () => {
    expect(parseTransitionLine(line({ taskId: "demo-task" }))).toBeNull();
  });

  it("normalizes a full transition line", () => {
    const out = parseTransitionLine(
      line({
        timestamp: "2026-06-28T00:00:00Z",
        taskId: "demo-task",
        from: "planned",
        to: "in_progress",
        actorRole: "implementer",
        note: "a transition note",
      }),
    );
    expect(out).toEqual({
      type: "transition",
      ts: "2026-06-28T00:00:00Z",
      taskId: "demo-task",
      from: "planned",
      to: "in_progress",
      actor: "implementer",
      note: "a transition note",
    });
  });

  it("defaults optional fields to empty strings when only taskId and to are present", () => {
    expect(parseTransitionLine(line({ taskId: "demo-task", to: "ready" }))).toEqual({
      type: "transition",
      ts: "",
      taskId: "demo-task",
      from: "",
      to: "ready",
      actor: "",
      note: "",
    });
  });
});

describe("clean", () => {
  it("maps a dropped (nullish) field to an empty string, never null", () => {
    expect(clean(null)).toBe("");
  });

  it("passes a clean field through unchanged", () => {
    expect(clean("a normal field")).toBe("a normal field");
  });
});

// commitEvent renders one git commit into a live-feed event: subject and
// shortstat read from git, both run through clean() so a home path or identity
// token in a subject never reaches the public feed, and the git wrapper fails
// closed to empty strings rather than throwing on a bad ref. Driven against a
// throwaway repo (hooks disabled so the project commit guard does not run).
describe("commitEvent", () => {
  let root: string;

  function sh(...args: string[]) {
    const env = {
      ...process.env,
      GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_CONFIG_SYSTEM: "/dev/null",
      GIT_AUTHOR_DATE: "2026-01-01T00:00:00",
      GIT_COMMITTER_DATE: "2026-01-01T00:00:00",
    };
    delete env.COVENANT_SESSION_ID;
    return execFileSync("git", ["-C", root, "-c", "core.hooksPath=/dev/null", ...args], {
      encoding: "utf8",
      env,
    });
  }

  function commit(msg: string, file: string, body: string) {
    writeFileSync(join(root, file), body);
    sh("add", "-A");
    sh("commit", "-q", "-m", msg);
    return sh("rev-parse", "--short", "HEAD").trim();
  }

  beforeEach(() => {
    root = mkdtempSync(join(tmpdir(), "cov-ce-"));
    sh("init", "-q", "-b", "main");
    sh("config", "user.name", "covtester");
    sh("config", "user.email", "cov@example.test");
  });

  afterEach(() => rmSync(root, { recursive: true, force: true }));

  it("returns the subject and shortstat for a commit", () => {
    const hash = commit("add ledger", "ledger.txt", "a\nb\n");
    const ev = commitEvent(root, hash);
    expect(ev.type).toBe("commit");
    expect(ev.hash).toBe(hash);
    expect(ev.subject).toBe("add ledger");
    expect(ev.stat).toMatch(/^1 file changed, 2 insertions\(\+\)$/);
  });

  it("redacts a home path in the subject", () => {
    const hash = commit("edit at /Users/secret/loc", "x.txt", "y\n");
    expect(commitEvent(root, hash).subject).toBe("edit at ~/loc");
  });

  it("fails closed to empty subject and stat on an unknown ref", () => {
    commit("seed", "x.txt", "y\n");
    expect(commitEvent(root, "deadbeef")).toEqual({
      type: "commit",
      hash: "deadbeef",
      subject: "",
      stat: "",
    });
  });
});
