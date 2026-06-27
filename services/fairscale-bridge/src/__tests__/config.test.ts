import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { loadConfig } from '../config.js';

// loadConfig() reads process.env directly (no injectable env), so each test
// installs a controlled env over a saved snapshot that afterEach restores. The
// baseline carries only the two fail-closed credentials; individual tests
// override a single var to isolate the arm under test.
const ENV_KEYS = [
  'FAIRSCALE_API_TOKEN',
  'COVENANT_OPERATOR_TOKEN',
  'PORT',
  'FAIRSCALE_BRIDGE_PORT',
  'COVENANT_DAEMON_URL',
  'COVENANT_DAEMON_TIMEOUT_MS',
  'COVENANT_DAEMON_RETRIES',
  'FAIRSCALE_BRIDGE_MAX_LIMIT',
  'FAIRSCALE_BRIDGE_DEFAULT_LIMIT',
  'FAIRSCALE_BRIDGE_FETCH_CAP',
] as const;

const TOKEN24 = 'a'.repeat(24); // shortest API token that clears the strength guard

let saved: Record<string, string | undefined>;

beforeEach(() => {
  saved = {};
  for (const k of ENV_KEYS) {
    saved[k] = process.env[k];
    delete process.env[k];
  }
  process.env.FAIRSCALE_API_TOKEN = TOKEN24;
  process.env.COVENANT_OPERATOR_TOKEN = 'operator-token';
});

afterEach(() => {
  for (const k of ENV_KEYS) {
    if (saved[k] === undefined) delete process.env[k];
    else process.env[k] = saved[k];
  }
});

describe('loadConfig', () => {
  it('loads a complete config with every default applied', () => {
    expect(loadConfig()).toEqual({
      port: 8788,
      daemonUrl: 'http://127.0.0.1:8421',
      daemonToken: 'operator-token',
      daemonTimeoutMs: 15_000,
      daemonRetries: 1,
      apiToken: TOKEN24,
      maxLimit: 1000,
      defaultLimit: 100,
      fetchCap: 100_000,
    });
  });

  describe('fail-closed credentials', () => {
    it('throws when FAIRSCALE_API_TOKEN is missing', () => {
      delete process.env.FAIRSCALE_API_TOKEN;
      expect(() => loadConfig()).toThrow(/missing required env FAIRSCALE_API_TOKEN/);
    });

    it('throws on a whitespace-only FAIRSCALE_API_TOKEN (blank is not present)', () => {
      process.env.FAIRSCALE_API_TOKEN = '   ';
      expect(() => loadConfig()).toThrow(/missing required env FAIRSCALE_API_TOKEN/);
    });

    it('throws when COVENANT_OPERATOR_TOKEN is missing', () => {
      delete process.env.COVENANT_OPERATOR_TOKEN;
      expect(() => loadConfig()).toThrow(/missing required env COVENANT_OPERATOR_TOKEN/);
    });
  });

  describe('API token strength guard', () => {
    it('rejects a present-but-short token (23 chars) — strength is distinct from presence', () => {
      process.env.FAIRSCALE_API_TOKEN = 'a'.repeat(23);
      expect(() => loadConfig()).toThrow(/FAIRSCALE_API_TOKEN must be at least 24 chars/);
    });

    it('accepts a token at the inclusive 24-char boundary', () => {
      process.env.FAIRSCALE_API_TOKEN = 'a'.repeat(24);
      expect(loadConfig().apiToken).toBe('a'.repeat(24));
    });
  });

  describe('defaultLimit clamp', () => {
    it('clamps defaultLimit down to maxLimit so a misconfigured default cannot exceed the operator max', () => {
      process.env.FAIRSCALE_BRIDGE_MAX_LIMIT = '1000';
      process.env.FAIRSCALE_BRIDGE_DEFAULT_LIMIT = '5000';
      const cfg = loadConfig();
      expect(cfg.maxLimit).toBe(1000);
      expect(cfg.defaultLimit).toBe(1000); // Math.min(5000, 1000), never 5000
    });

    it('keeps defaultLimit untouched when it is already below maxLimit', () => {
      process.env.FAIRSCALE_BRIDGE_MAX_LIMIT = '1000';
      process.env.FAIRSCALE_BRIDGE_DEFAULT_LIMIT = '100';
      expect(loadConfig().defaultLimit).toBe(100); // Math.max would wrongly hoist this to 1000
    });
  });

  describe('int() min-boundary policy', () => {
    it('rejects FAIRSCALE_BRIDGE_MAX_LIMIT=0 (default int floor is 1)', () => {
      process.env.FAIRSCALE_BRIDGE_MAX_LIMIT = '0';
      expect(() => loadConfig()).toThrow(/FAIRSCALE_BRIDGE_MAX_LIMIT must be an integer >= 1/);
    });

    it('accepts COVENANT_DAEMON_RETRIES=0 — retries are explicitly allowed to be zero (min 0, not 1)', () => {
      process.env.COVENANT_DAEMON_RETRIES = '0';
      expect(loadConfig().daemonRetries).toBe(0);
    });

    it('rejects a fractional COVENANT_DAEMON_TIMEOUT_MS (integer only)', () => {
      process.env.COVENANT_DAEMON_TIMEOUT_MS = '15.5';
      expect(() => loadConfig()).toThrow(/COVENANT_DAEMON_TIMEOUT_MS must be an integer >= 1/);
    });
  });

  describe('port precedence', () => {
    it('prefers PORT over FAIRSCALE_BRIDGE_PORT', () => {
      process.env.FAIRSCALE_BRIDGE_PORT = '9000';
      process.env.PORT = '9100';
      expect(loadConfig().port).toBe(9100);
    });

    it('falls back to FAIRSCALE_BRIDGE_PORT when PORT is unset', () => {
      process.env.FAIRSCALE_BRIDGE_PORT = '9000';
      expect(loadConfig().port).toBe(9000);
    });
  });

  describe('daemon URL normalization', () => {
    it('strips trailing slashes so request paths do not double up', () => {
      process.env.COVENANT_DAEMON_URL = 'http://daemon.internal:8421///';
      expect(loadConfig().daemonUrl).toBe('http://daemon.internal:8421');
    });
  });
});
