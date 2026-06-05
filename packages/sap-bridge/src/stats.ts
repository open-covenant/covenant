// Best-effort RPC usage counters for the Synapse bridge.
//
// The worker is spawned once per operation, so in-process counters would
// not survive between invocations. We persist a tiny JSON file
// (read-modify-write, atomic rename) so successive worker runs — and the
// daemon's /sap/stats reader on the same host — see cumulative totals.
//
// Every network-bearing command is counted; `status` and `stats`
// (no RPC) are excluded by the caller. Writes are strictly best-effort:
// telemetry must never fail an on-chain operation.

import { mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

export interface RpcStats {
  calls: number;
  successes: number;
  failures: number;
  // Unix seconds of the first recorded call — the anchor for "uptime".
  firstSeenUnix: number;
  lastCallUnix: number;
}

const EMPTY: RpcStats = {
  calls: 0,
  successes: 0,
  failures: 0,
  firstSeenUnix: 0,
  lastCallUnix: 0,
};

// Resolve the stats file path. Explicit override wins; otherwise it
// lives under COVENANT_HOME (the daemon's data dir), falling back to
// ~/.covenant. The Rust daemon resolves the identical path so its
// /sap/stats route reads the same file the worker writes.
export function statsPath(env: NodeJS.ProcessEnv = process.env): string {
  const override = env.COVENANT_SAP_STATS_PATH;
  if (override && override.trim()) return override.trim();
  const home = env.COVENANT_HOME && env.COVENANT_HOME.trim()
    ? env.COVENANT_HOME.trim()
    : join(homedir(), '.covenant');
  return join(home, 'sap-rpc-stats.json');
}

export function readStats(env: NodeJS.ProcessEnv = process.env): RpcStats {
  try {
    const parsed = JSON.parse(readFileSync(statsPath(env), 'utf8')) as Partial<RpcStats>;
    return { ...EMPTY, ...parsed };
  } catch {
    return { ...EMPTY };
  }
}

export function recordCall(ok: boolean, env: NodeJS.ProcessEnv = process.env): void {
  try {
    const path = statsPath(env);
    const now = Math.floor(Date.now() / 1000);
    const cur = readStats(env);
    const next: RpcStats = {
      calls: cur.calls + 1,
      successes: cur.successes + (ok ? 1 : 0),
      failures: cur.failures + (ok ? 0 : 1),
      firstSeenUnix: cur.firstSeenUnix || now,
      lastCallUnix: now,
    };
    mkdirSync(dirname(path), { recursive: true });
    // Atomic publish: write a pid-scoped temp then rename over the live
    // file so a concurrent reader never sees a half-written JSON.
    const tmp = `${path}.${process.pid}.tmp`;
    writeFileSync(tmp, `${JSON.stringify(next)}\n`, 'utf8');
    renameSync(tmp, path);
  } catch {
    // Best-effort: swallow so the counter file can never break an op.
  }
}
