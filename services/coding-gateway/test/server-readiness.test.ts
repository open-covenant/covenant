import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const token = 'g'.repeat(32);
const originalToken = process.env.CODER_AUTH_TOKEN;
const originalNodeEnv = process.env.NODE_ENV;

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
});
