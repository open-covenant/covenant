import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

export function runStage(stage, repoRoot) {
  const cwd = stage.cwd ? resolve(repoRoot, stage.cwd) : repoRoot;
  const start = Date.now();
  const r = spawnSync(stage.cmd[0], stage.cmd.slice(1), {
    cwd,
    encoding: "utf8",
    timeout: stage.timeoutMs ?? 600000,
    maxBuffer: 64 * 1024 * 1024,
  });
  return {
    ok: r.status === 0,
    status: r.status,
    ms: Date.now() - start,
    out: `${r.stdout ?? ""}${r.stderr ?? ""}`,
  };
}

export function testFraction(out) {
  let passed = 0;
  let failed = 0;
  for (const m of out.matchAll(/test result:\s+\w+\.\s+(\d+)\s+passed;\s+(\d+)\s+failed/g)) {
    passed += Number(m[1]);
    failed += Number(m[2]);
  }
  const total = passed + failed;
  return { passed, failed, fraction: total ? passed / total : 0 };
}
