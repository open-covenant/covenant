import { describe, it, expect, afterEach } from 'vitest';
import { existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readStats, recordCall, statsPath } from '../stats.js';

// Each test drives an isolated counter file via COVENANT_SAP_STATS_PATH
// so the suite never touches a real ~/.covenant file.
function tmpEnv(name: string): NodeJS.ProcessEnv {
  return { COVENANT_SAP_STATS_PATH: join(tmpdir(), `sap-stats-test-${name}-${process.pid}.json`) };
}

const created: string[] = [];
afterEach(() => {
  for (const p of created.splice(0)) {
    if (existsSync(p)) rmSync(p);
  }
});

describe('rpc stats counters', () => {
  it('reads zeros when the file is absent', () => {
    const env = tmpEnv('absent');
    created.push(statsPath(env));
    const stats = readStats(env);
    expect(stats).toEqual({
      calls: 0,
      successes: 0,
      failures: 0,
      firstSeenUnix: 0,
      lastCallUnix: 0,
    });
  });

  it('accumulates successes and failures across calls', () => {
    const env = tmpEnv('accumulate');
    created.push(statsPath(env));
    recordCall(true, env);
    recordCall(true, env);
    recordCall(false, env);
    const stats = readStats(env);
    expect(stats.calls).toBe(3);
    expect(stats.successes).toBe(2);
    expect(stats.failures).toBe(1);
    expect(stats.firstSeenUnix).toBeGreaterThan(0);
    expect(stats.lastCallUnix).toBeGreaterThanOrEqual(stats.firstSeenUnix);
  });

  it('preserves firstSeenUnix once set', () => {
    const env = tmpEnv('first-seen');
    created.push(statsPath(env));
    recordCall(true, env);
    const first = readStats(env).firstSeenUnix;
    recordCall(true, env);
    expect(readStats(env).firstSeenUnix).toBe(first);
  });

  it('honors COVENANT_SAP_STATS_PATH override', () => {
    const env = tmpEnv('override');
    expect(statsPath(env)).toBe(env.COVENANT_SAP_STATS_PATH);
  });
});
