import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

const script = fileURLToPath(new URL("./verify-run.mjs", import.meta.url));
const sha = "a".repeat(40);
const roots = [];

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "covenant-verify-run-"));
  roots.push(root);
  const home = join(root, "home");
  const repo = join(root, "repo");
  const audit = join(home, "audit", "events.jsonl");
  mkdirSync(dirname(audit), { recursive: true });
  writeFileSync(audit, '{"id":"event-1","kind":{"type":"skill_installed"}}\n');
  return { home, repo };
}

function run(home, repo, commit = sha) {
  return spawnSync(
    process.execPath,
    [script, "--home", home, "--sha", commit, "--repo", repo],
    { encoding: "utf8" },
  );
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe("verify-run", () => {
  it("publishes commit-scoped witness outputs only once", () => {
    const { home, repo } = fixture();
    const first = run(home, repo);
    expect(first.status, first.stderr).toBe(0);

    const keyPath = join(
      repo,
      "landing",
      "public",
      "witness",
      "verifier-keys",
      `${sha}.txt`,
    );
    const firstKey = readFileSync(keyPath, "utf8");

    const second = run(home, repo);
    expect(second.status, second.stderr).toBe(0);
    expect(readFileSync(keyPath, "utf8")).toBe(firstKey);

    writeFileSync(
      join(home, "audit", "events.jsonl"),
      '{"id":"event-2","kind":{"type":"skill_installed"}}\n',
    );
    const conflicting = run(home, repo);
    expect(conflicting.status).not.toBe(0);
    expect(`${conflicting.stdout}\n${conflicting.stderr}`).toContain(
      "refusing to replace commit-scoped witness output",
    );
    expect(readFileSync(keyPath, "utf8")).toBe(firstKey);
  });

  it("rejects a path-like or abbreviated commit id", () => {
    const { home, repo } = fixture();
    const result = run(home, repo, "../../replacement");
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("full lowercase 40-character Git commit id");
  });
});
