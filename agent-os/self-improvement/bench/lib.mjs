import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

export const round = (n) => Math.round(n * 1000) / 1000;

export function sh(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...opts });
  return { ok: r.status === 0, status: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

export function runStage(stage, cwd) {
  const dir = stage.cwd ? resolve(cwd, stage.cwd) : cwd;
  const start = Date.now();
  const r = spawnSync(stage.cmd[0], stage.cmd.slice(1), {
    cwd: dir,
    encoding: "utf8",
    timeout: stage.timeoutMs ?? 600000,
    maxBuffer: 64 * 1024 * 1024,
  });
  return { ok: r.status === 0, status: r.status, ms: Date.now() - start, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
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
