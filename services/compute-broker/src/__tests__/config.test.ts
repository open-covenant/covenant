import { describe, it, expect } from 'vitest';
import { loadConfig } from '../config.js';

// loadConfig takes an optional env object (defaults to process.env), so each
// test passes a controlled env. zod parse throws ZodError on any constraint
// failure, so invalid values are asserted as synchronous throws.

describe('loadConfig', () => {
  it('applies every default from an empty env', () => {
    const cfg = loadConfig({});
    expect(cfg.port).toBe(8788);
    expect(cfg.host).toBe('0.0.0.0');
    expect(cfg.ionetApiUrl).toBe('https://api.io.net/v1');
    expect(cfg.akashRpcUrl).toBe('https://rpc.akash.network');
    expect(cfg.minBondMicro).toBe(10_000_000);
  });

  describe('PORT range', () => {
    it('rejects 0 (below the min(1) floor)', () => {
      expect(() => loadConfig({ PORT: '0' })).toThrow();
    });

    it('accepts the 1 boundary', () => {
      expect(loadConfig({ PORT: '1' }).port).toBe(1);
    });

    it('rejects 65536 (above the max(65535) ceiling)', () => {
      expect(() => loadConfig({ PORT: '65536' })).toThrow();
    });

    it('rejects a fractional port (int only)', () => {
      expect(() => loadConfig({ PORT: '1.5' })).toThrow();
    });
  });

  describe('positive() fields', () => {
    it('rejects a zero rotation grace (positive, not nonnegative)', () => {
      expect(() => loadConfig({ BROKER_KEY_ROTATION_GRACE_SECS: '0' })).toThrow();
    });

    it('rejects a zero min bond (positive)', () => {
      expect(() => loadConfig({ MIN_BOND_USD_MICRO: '0' })).toThrow();
    });

    it('rejects a zero bond-cancel expiry window (positive)', () => {
      expect(() => loadConfig({ BOND_CANCEL_MAX_EXPIRY_SECS: '0' })).toThrow();
    });
  });

  describe('url fields', () => {
    it('rejects a non-URL io.net api url', () => {
      expect(() => loadConfig({ IONET_API_URL: 'not-a-url' })).toThrow();
    });

    it('rejects a non-URL akash rpc url', () => {
      expect(() => loadConfig({ AKASH_RPC_URL: 'also-not-a-url' })).toThrow();
    });
  });

  describe('OPERATOR_BEARER_TOKEN min length', () => {
    it('rejects a 15-char bearer (min 16) when set', () => {
      expect(() => loadConfig({ OPERATOR_BEARER_TOKEN: 'x'.repeat(15) })).toThrow();
    });

    it('accepts a 16-char bearer', () => {
      expect(loadConfig({ OPERATOR_BEARER_TOKEN: 'x'.repeat(16) }).operatorBearerToken).toBe('x'.repeat(16));
    });

    it('accepts an unset bearer (optional)', () => {
      expect(loadConfig({}).operatorBearerToken).toBeUndefined();
    });
  });
});
