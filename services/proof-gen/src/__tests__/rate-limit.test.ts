import { describe, it, expect } from 'vitest';
import { checkRateLimit, rateLimitKey, RateLimitBackendError, type RateLimitDeps } from '../rate-limit.js';
import type IORedis from 'ioredis';

// The rate-limit decision is computed by a Redis Lua script; the TS layer just
// interprets the [allowed, ttl] tuple it returns. Drive that interpretation by
// injecting a fake connection whose eval() returns a canned tuple (or throws).
function fakeConn(behavior: () => [number, number]): IORedis {
  return { eval: async () => behavior() } as unknown as IORedis;
}
function failingConn(err: Error): IORedis {
  return { eval: async () => { throw err; } } as unknown as IORedis;
}

const deps = (connection: IORedis, over: Partial<RateLimitDeps> = {}): RateLimitDeps => ({
  connection,
  limit: 5,
  windowMs: 60_000,
  ...over,
});

describe('checkRateLimit', () => {
  it('returns ok when the backend allows (raw[0] === 1)', async () => {
    const outcome = await checkRateLimit('did:agent', deps(fakeConn(() => [1, 0])));
    expect(outcome).toEqual({ ok: true });
  });

  it('converts a sub-second-denied ttl to a retry_after of ceil(ttl/1000) seconds', async () => {
    const outcome = await checkRateLimit('did:agent', deps(fakeConn(() => [0, 2500])));
    expect(outcome).toEqual({ ok: false, retry_after: 3 });
  });

  it('floors retry_after at 1s so a zero/sub-second ttl never yields a 0s tight-retry loop', async () => {
    const outcome = await checkRateLimit('did:agent', deps(fakeConn(() => [0, 0])));
    expect(outcome).toEqual({ ok: false, retry_after: 1 });
  });

  it('fails closed on a backend error instead of silently allowing', async () => {
    const cause = new Error('ECONNREFUSED');
    const call = () => checkRateLimit('did:agent', deps(failingConn(cause)));
    await expect(call()).rejects.toBeInstanceOf(RateLimitBackendError);
    await expect(call()).rejects.toThrow(/ECONNREFUSED/);
  });
});

describe('rateLimitKey', () => {
  it('namespaces the bucket under the proof-gen rate-limit prefix', () => {
    expect(rateLimitKey('did:agent')).toBe('proof-gen:rl:did:agent');
  });
});
