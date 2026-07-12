import { describe, it, expect, afterEach } from 'vitest';
import { existsSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
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

// The path resolver and the partial-file merge are the two arms a worker and
// the daemon's /sap/stats reader both depend on to agree on one file: an
// override wins only when non-blank (and is trimmed), COVENANT_HOME is the next
// fallback (also trimmed), then ~/.covenant; a read defaults missing fields so
// an older or partial file never surfaces undefined counters.
describe('statsPath resolution', () => {
  it('ignores an empty or whitespace-only override and falls through to COVENANT_HOME', () => {
    const home = join(tmpdir(), `sap-home-${process.pid}`);
    expect(statsPath({ COVENANT_SAP_STATS_PATH: '', COVENANT_HOME: home })).toBe(
      join(home, 'sap-rpc-stats.json'),
    );
    expect(statsPath({ COVENANT_SAP_STATS_PATH: '   ', COVENANT_HOME: home })).toBe(
      join(home, 'sap-rpc-stats.json'),
    );
  });

  it('trims surrounding whitespace from a real override', () => {
    const target = join(tmpdir(), `sap-explicit-${process.pid}.json`);
    expect(statsPath({ COVENANT_SAP_STATS_PATH: `  ${target}  ` })).toBe(target);
  });

  it('resolves under a trimmed COVENANT_HOME when no override is set', () => {
    const home = join(tmpdir(), `sap-home-${process.pid}`);
    expect(statsPath({ COVENANT_HOME: home })).toBe(join(home, 'sap-rpc-stats.json'));
    expect(statsPath({ COVENANT_HOME: `  ${home}  ` })).toBe(join(home, 'sap-rpc-stats.json'));
  });

  it('falls back to ~/.covenant when neither override nor COVENANT_HOME is usable', () => {
    const expected = join(homedir(), '.covenant', 'sap-rpc-stats.json');
    expect(statsPath({})).toBe(expected);
    expect(statsPath({ COVENANT_HOME: '   ' })).toBe(expected);
  });
});

describe('readStats partial-file merge', () => {
  it('defaults missing fields from the all-zero baseline (forward/backward compat)', () => {
    const env = tmpEnv('partial');
    const path = statsPath(env);
    created.push(path);
    writeFileSync(path, JSON.stringify({ calls: 5, successes: 4 }), 'utf8');
    expect(readStats(env)).toEqual({
      calls: 5,
      successes: 4,
      failures: 0,
      firstSeenUnix: 0,
      lastCallUnix: 0,
    });
  });
});
