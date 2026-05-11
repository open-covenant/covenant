import { describe, expect, it } from 'vitest';
import RedisMock from 'ioredis-mock';
import { cacheKey, resultKey } from '../queue.js';

describe('cache key shape', () => {
  it('cacheKey is distinct from resultKey', () => {
    expect(cacheKey('abc')).toBe('proof-gen:cache:abc');
    expect(resultKey('xyz')).toBe('proof-gen:result:xyz');
    expect(cacheKey('x') === resultKey('x')).toBe(false);
  });
});

describe('idempotency behavior', () => {
  it('GET on cache miss returns null', async () => {
    const r = new RedisMock();
    expect(await r.get(cacheKey('missing'))).toBeNull();
    await r.quit();
  });

  it('worker-side SET persists; server-side GET returns payload', async () => {
    const r = new RedisMock();
    const hash = 'deadbeef';
    const payload = JSON.stringify({ status: 'completed', proof: { pi_a: [] }, public_signals: [] });
    await r.set(cacheKey(hash), payload, 'EX', 60);
    const got = await r.get(cacheKey(hash));
    expect(got).toBe(payload);
    await r.quit();
  });

  it('TTL expiry evicts cache entry', async () => {
    const r = new RedisMock();
    await r.set(cacheKey('h'), 'v', 'PX', 10);
    await new Promise((res) => setTimeout(res, 25));
    expect(await r.get(cacheKey('h'))).toBeNull();
    await r.quit();
  });
});
