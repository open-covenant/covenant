import { mkdtempSync, readFileSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  applyEnvOverrides,
  defaultConfig,
  deskHome,
  deskPaths,
  liveBlockedReason,
  loadConfig,
  parseConfig,
  saveConfig,
} from '../config.js';
import { ConfigError } from '../errors.js';

const homes: string[] = [];

function tempHome(): NodeJS.ProcessEnv {
  const home = mkdtempSync(path.join(tmpdir(), 'desk-config-'));
  homes.push(home);
  return { DESK_HOME: home } as NodeJS.ProcessEnv;
}

afterEach(() => {
  homes.length = 0;
});

describe('paths', () => {
  it('honours DESK_HOME', () => {
    const env = { DESK_HOME: '/tmp/desk-one' } as NodeJS.ProcessEnv;
    expect(deskHome(env)).toBe('/tmp/desk-one');
    expect(deskPaths(env).config).toBe('/tmp/desk-one/config.json');
    expect(deskPaths(env).keys).toBe('/tmp/desk-one/keys.env');
    expect(deskPaths(env).database).toBe('/tmp/desk-one/desk.sqlite');
  });

  it('falls back to XDG_CONFIG_HOME, then the home directory', () => {
    expect(deskHome({ XDG_CONFIG_HOME: '/tmp/xdg' } as NodeJS.ProcessEnv)).toBe('/tmp/xdg/covenant-desk');
    expect(deskHome({} as NodeJS.ProcessEnv)).toMatch(/\.config\/covenant-desk$/);
  });
});

describe('defaults', () => {
  it('ships dry run with the documented bounds', () => {
    const config = defaultConfig();
    expect(config.live).toBe(false);
    expect(config.acknowledgedRestrictions).toBe(false);
    expect(config.chainId).toBe(4663);
    expect(config.port).toBe(46631);
    expect(config.rpcUrl).toBe('https://rpc.mainnet.chain.robinhood.com');
    expect(config.bounds).toEqual({
      maxOrderNotionalUsd: 250,
      maxDailyNotionalUsd: 1000,
      maxSlippageBps: 100,
      maxBuyPremiumBps: 500,
    });
    expect(config.recorderIntervalSec).toBe(60);
    expect(config.hedge.driftBps).toBe(500);
    expect(config.token).toHaveLength(64);
  });

  it('generates a different token each time', () => {
    expect(defaultConfig().token).not.toBe(defaultConfig().token);
  });
});

describe('save and load', () => {
  it('writes mode 600 and reads back the same values', () => {
    const env = tempHome();
    const config = defaultConfig({ port: 47000 });
    const file = saveConfig(config, env);
    expect(statSync(file).mode & 0o777).toBe(0o600);
    expect(JSON.parse(readFileSync(file, 'utf8')).port).toBe(47000);
    const loaded = loadConfig(env);
    expect(loaded.port).toBe(47000);
    expect(loaded.token).toBe(config.token);
  });

  it('names the file when there is no config', () => {
    const env = tempHome();
    expect(() => loadConfig(env)).toThrow(ConfigError);
    try {
      loadConfig(env);
    } catch (error) {
      expect((error as ConfigError).reason).toContain('covenant-desk init');
    }
  });
});

describe('validation', () => {
  it('refuses a short bearer token', () => {
    expect(() => parseConfig(JSON.stringify({ token: 'short' }))).toThrow(ConfigError);
  });

  it('refuses a port outside range', () => {
    expect(() => parseConfig(JSON.stringify({ token: 'x'.repeat(32), port: 99999 }))).toThrow(ConfigError);
  });

  it('reports which field failed', () => {
    try {
      parseConfig(JSON.stringify({ token: 'x'.repeat(32), bounds: { maxSlippageBps: -5 } }));
      throw new Error('expected a ConfigError');
    } catch (error) {
      expect((error as ConfigError).reason).toContain('bounds.maxSlippageBps');
    }
  });

  it('refuses text that is not JSON', () => {
    expect(() => parseConfig('{')).toThrow(ConfigError);
  });
});

describe('environment overrides', () => {
  it('replaces the endpoint, port, token, and log level', () => {
    const config = applyEnvOverrides(defaultConfig(), {
      DESK_RPC_URL: 'http://127.0.0.1:8545',
      DESK_PORT: '5000',
      DESK_TOKEN: 'y'.repeat(40),
      DESK_LOG_LEVEL: 'debug',
    } as NodeJS.ProcessEnv);
    expect(config.rpcUrl).toBe('http://127.0.0.1:8545');
    expect(config.port).toBe(5000);
    expect(config.token).toBe('y'.repeat(40));
    expect(config.logLevel).toBe('debug');
  });

  it('reads the environment it was handed, not the one the process was started with', () => {
    const config = parseConfig(JSON.stringify(defaultConfig()), '<memory>', {
      DESK_PORT: '5001',
    } as NodeJS.ProcessEnv);
    expect(config.port).toBe(5001);
  });

  it('refuses a port that is not a port', () => {
    expect(() => applyEnvOverrides(defaultConfig(), { DESK_PORT: 'eight' } as NodeJS.ProcessEnv)).toThrow(ConfigError);
  });
});

describe('liveBlockedReason', () => {
  it('names the acknowledgement first', () => {
    const reason = liveBlockedReason(defaultConfig({ live: true }));
    expect(reason).toContain('acknowledge-restrictions');
  });

  it('names dry run once restrictions are acknowledged', () => {
    const reason = liveBlockedReason(defaultConfig({ acknowledgedRestrictions: true }));
    expect(reason).toContain('dry run');
  });

  it('returns null when both are set', () => {
    expect(liveBlockedReason(defaultConfig({ live: true, acknowledgedRestrictions: true }))).toBeNull();
  });
});
