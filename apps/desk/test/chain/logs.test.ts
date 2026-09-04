import { describe, expect, it } from 'vitest';
import {
  chunkRange,
  DEFAULT_SPAN,
  isChallengeError,
  isLogLimitError,
  isQueryTimeoutError,
  isRangeTooWideError,
  isRateLimitError,
  rangeSize,
  splitRange,
} from '../../src/chain/logs.js';

describe('chunkRange', () => {
  it('covers the range exactly, with no gap and no overlap', () => {
    const ranges = chunkRange(0n, 9n, 4n);
    expect(ranges).toEqual([
      { from: 0n, to: 3n },
      { from: 4n, to: 7n },
      { from: 8n, to: 9n },
    ]);
    expect(ranges.reduce((total, range) => total + rangeSize(range), 0n)).toBe(10n);
  });

  it('returns one range when the span is wider than the request', () => {
    expect(chunkRange(100n, 200n, 1_000n)).toEqual([{ from: 100n, to: 200n }]);
  });

  it('returns nothing when there is nothing to scan', () => {
    expect(chunkRange(10n, 9n)).toEqual([]);
  });

  it('splits the live chain tip into the span the node serves', () => {
    const ranges = chunkRange(0n, 54_444_943n);
    expect(ranges).toHaveLength(28);
    expect(ranges[0]).toEqual({ from: 0n, to: DEFAULT_SPAN - 1n });
    expect(ranges.at(-1)?.to).toBe(54_444_943n);
  });

  it('refuses a span of zero', () => {
    expect(() => chunkRange(0n, 10n, 0n)).toThrow(RangeError);
  });
});

describe('splitRange', () => {
  it('halves a range and keeps both ends', () => {
    expect(splitRange({ from: 0n, to: 99n })).toEqual([
      { from: 0n, to: 49n },
      { from: 50n, to: 99n },
    ]);
  });

  it('halves an odd range without dropping a block', () => {
    const halves = splitRange({ from: 7n, to: 12n });
    expect(halves).not.toBeNull();
    const [left, right] = halves ?? [];
    expect(left?.from).toBe(7n);
    expect(right?.to).toBe(12n);
    expect((left?.to ?? 0n) + 1n).toBe(right?.from);
  });

  it('cannot split a single block', () => {
    expect(splitRange({ from: 42n, to: 42n })).toBeNull();
  });
});

describe('error classification', () => {
  it('recognises the ten-thousand log refusal', () => {
    const error = new Error('logs matched by query exceeds limit of 10000');
    expect(isLogLimitError(error)).toBe(true);
    expect(isRangeTooWideError(error)).toBe(true);
    expect(isRateLimitError(error)).toBe(false);
  });

  it('recognises the query timeout refusal', () => {
    const error = Object.assign(new Error('RPC Request failed.'), {
      details: 'log query timed out',
    });
    expect(isQueryTimeoutError(error)).toBe(true);
    expect(isRangeTooWideError(error)).toBe(true);
  });

  it('reads a Cloudflare challenge as a signal to slow down', () => {
    // What the Robinhood Chain RPC answers a client asking too fast, trimmed.
    const error = Object.assign(new Error('HTTP request failed.'), {
      status: 403,
      details:
        '<!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title>' +
        '<script>window._cf_chl_opt = {cZone: \'rpc.mainnet.chain.robinhood.com\'};</script>',
    });
    expect(isChallengeError(error)).toBe(true);
    expect(isRateLimitError(error)).toBe(true);
    expect(isRangeTooWideError(error)).toBe(false);
  });

  it('leaves an ordinary refusal alone', () => {
    const error = Object.assign(new Error('HTTP request failed.'), {
      status: 403,
      details: 'Forbidden: this API key is not authorised for eth_getLogs',
    });
    expect(isChallengeError(error)).toBe(false);
    expect(isRateLimitError(error)).toBe(false);
  });

  it('recognises a rate limit, which narrowing would not fix', () => {
    const error = Object.assign(new Error('RPC Request failed.'), {
      code: 429,
      cause: { code: 429, message: 'Too Many Requests' },
    });
    expect(isRateLimitError(error)).toBe(true);
    expect(isRangeTooWideError(error)).toBe(false);
  });

  it('leaves an unrelated failure alone', () => {
    const error = new Error('connect ECONNREFUSED 127.0.0.1:8545');
    expect(isRangeTooWideError(error)).toBe(false);
    expect(isRateLimitError(error)).toBe(false);
  });
});
