import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const token = 'g'.repeat(32);
const originalToken = process.env.CODER_AUTH_TOKEN;
const originalNodeEnv = process.env.NODE_ENV;
const originalBackend = process.env.CODER_BACKEND;
const originalModel = process.env.CODER_MODEL;
const originalUsePodKey = process.env.USEPOD_API_KEY;
const originalMinimumBalance = process.env.USEPOD_MIN_BALANCE;
const nativeFetch = globalThis.fetch;

beforeEach(() => {
  process.env.NODE_ENV = 'test';
  process.env.CODER_AUTH_TOKEN = token;
  vi.resetModules();
});

afterEach(() => {
  if (originalToken === undefined) delete process.env.CODER_AUTH_TOKEN;
  else process.env.CODER_AUTH_TOKEN = originalToken;
  if (originalNodeEnv === undefined) delete process.env.NODE_ENV;
  else process.env.NODE_ENV = originalNodeEnv;
  if (originalBackend === undefined) delete process.env.CODER_BACKEND;
  else process.env.CODER_BACKEND = originalBackend;
  if (originalModel === undefined) delete process.env.CODER_MODEL;
  else process.env.CODER_MODEL = originalModel;
  if (originalUsePodKey === undefined) delete process.env.USEPOD_API_KEY;
  else process.env.USEPOD_API_KEY = originalUsePodKey;
  if (originalMinimumBalance === undefined) delete process.env.USEPOD_MIN_BALANCE;
  else process.env.USEPOD_MIN_BALANCE = originalMinimumBalance;
  vi.unstubAllGlobals();
});

describe('gateway readiness endpoint', () => {
  it('keeps active dependency evidence behind the service token', async () => {
    const { server } = await import('../src/server.js');
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('gateway did not bind');
    const origin = `http://127.0.0.1:${address.port}`;

    try {
      const missing = await fetch(`${origin}/readyz`);
      expect(missing.status).toBe(401);
      await expect(missing.json()).resolves.toEqual({ error: 'unauthorized' });

      const wrong = await fetch(`${origin}/readyz`, {
        headers: { authorization: 'Bearer wrong-token' },
      });
      expect(wrong.status).toBe(401);

      const health = await fetch(`${origin}/healthz`);
      expect(health.status).toBe(200);
    } finally {
      await new Promise<void>((resolve, reject) => {
        server.close((cause) => (cause ? reject(cause) : resolve()));
      });
    }
  });

  it('returns 503 when the UsePod account is below USEPOD_MIN_BALANCE', async () => {
    process.env.CODER_BACKEND = 'usepod';
    process.env.CODER_MODEL = 'openai/gpt-oss-120b';
    process.env.USEPOD_API_KEY = 'test-token';
    process.env.USEPOD_MIN_BALANCE = '2000000';
    const providerFetch = vi.fn<typeof fetch>(async (input, init) => {
      const url = new URL(String(input));
      if (url.hostname !== 'api.usepod.ai') return nativeFetch(input, init);
      if (url.pathname.endsWith('/v1/models')) {
        return Response.json({
          object: 'list',
          data: [{ id: 'openai/gpt-oss-120b' }],
        });
      }
      if (url.pathname.endsWith('/balance')) {
        return Response.json(
          { usdc_balance: 1_999_999 },
          { headers: { 'x-balance-remaining': '1999999' } },
        );
      }
      return new Response(null, { status: 404 });
    });
    vi.stubGlobal('fetch', providerFetch);
    vi.resetModules();

    const { server } = await import('../src/server.js');
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('gateway did not bind');

    try {
      const response = await nativeFetch(`http://127.0.0.1:${address.port}/readyz`, {
        headers: { authorization: `Bearer ${token}` },
      });
      expect(response.status).toBe(503);
      await expect(response.json()).resolves.toMatchObject({
        ready: false,
        dependencies: { balance: { ok: false } },
        failed: expect.arrayContaining(['balance']),
      });
    } finally {
      await new Promise<void>((resolve, reject) => {
        server.close((cause) => (cause ? reject(cause) : resolve()));
      });
    }
  });
});
