import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AddressInfo } from 'node:net';
import type { Server } from 'node:http';
import { createFacilitatorServer, type FacilitatorApi } from './server.js';

const TOKEN = 'a'.repeat(48);
const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(
    servers
      .splice(0)
      .map((server) => new Promise<void>((resolve) => server.close(() => resolve()))),
  );
});

async function serve(api: Partial<FacilitatorApi> = {}): Promise<string> {
  const server = createFacilitatorServer(
    {
      supported: () => ({ kinds: [] }),
      verify: async () => ({ isValid: true, payer: 'payer' }),
      settle: async () => ({ success: true, transaction: 'signature' }),
      ...api,
    },
    { token: TOKEN, maxRequestBytes: 4_096 },
    () => {},
  );
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return `http://127.0.0.1:${port}`;
}

const body = JSON.stringify({ paymentPayload: { a: 1 }, paymentRequirements: { b: 2 } });

describe('facilitator http surface', () => {
  it('serves health without a token so the platform probe works', async () => {
    const base = await serve();

    const response = await fetch(`${base}/healthz`);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ ok: true });
  });

  it.each(['/supported', '/verify', '/settle'])('refuses %s without the token', async (path) => {
    const base = await serve();

    const response = await fetch(`${base}${path}`, {
      method: path === '/supported' ? 'GET' : 'POST',
      headers: { 'content-type': 'application/json' },
      body: path === '/supported' ? undefined : body,
    });

    expect(response.status).toBe(401);
  });

  it('refuses a token that is close but wrong', async () => {
    const base = await serve();

    const response = await fetch(`${base}/supported`, {
      headers: { authorization: `Bearer ${'a'.repeat(47)}b` },
    });

    expect(response.status).toBe(401);
  });

  it('verifies and settles for the runtime', async () => {
    const verify = vi.fn(async () => ({ isValid: true, payer: 'payer' }));
    const settle = vi.fn(async () => ({ success: true, transaction: 'signature' }));
    const base = await serve({ verify, settle });

    const verified = await fetch(`${base}/verify`, {
      method: 'POST',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
      body,
    });
    const settled = await fetch(`${base}/settle`, {
      method: 'POST',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
      body,
    });

    await expect(verified.json()).resolves.toEqual({ isValid: true, payer: 'payer' });
    await expect(settled.json()).resolves.toEqual({ success: true, transaction: 'signature' });
    expect(verify).toHaveBeenCalledWith({ a: 1 }, { b: 2 });
    expect(settle).toHaveBeenCalledWith({ a: 1 }, { b: 2 });
  });

  it('rejects a request that omits the payment requirements', async () => {
    const base = await serve();

    const response = await fetch(`${base}/verify`, {
      method: 'POST',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
      body: JSON.stringify({ paymentPayload: { a: 1 } }),
    });

    expect(response.status).toBe(400);
  });

  it('rejects a body beyond the configured limit', async () => {
    const base = await serve();

    const response = await fetch(`${base}/verify`, {
      method: 'POST',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
      body: JSON.stringify({ paymentPayload: 'x'.repeat(8_192), paymentRequirements: {} }),
    });

    expect(response.status).toBe(413);
  });

  it('reports a settlement failure without leaking the internal error', async () => {
    const base = await serve({
      settle: async () => {
        throw new Error('rpc credentials rejected');
      },
    });

    const response = await fetch(`${base}/settle`, {
      method: 'POST',
      headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
      body,
    });

    expect(response.status).toBe(500);
    await expect(response.text()).resolves.not.toContain('rpc credentials');
  });

  it('does not answer unknown routes', async () => {
    const base = await serve();

    const response = await fetch(`${base}/v1/anything`, {
      headers: { authorization: `Bearer ${TOKEN}` },
    });

    expect(response.status).toBe(404);
  });
});

describe('facilitator readiness', () => {
  it('reports unready when the fee payer cannot cover settlements', async () => {
    const base = await serve({ readiness: async () => ({ feePayerLamports: 1_000 }) });

    const response = await fetch(`${base}/readyz`);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toMatchObject({ ok: false });
  });

  it('reports ready when the fee payer is funded', async () => {
    const base = await serve({ readiness: async () => ({ feePayerLamports: 70_000_000 }) });

    const response = await fetch(`${base}/readyz`);

    expect(response.status).toBe(200);
  });

  it('reports unready when the chain cannot be reached', async () => {
    const base = await serve({
      readiness: async () => {
        throw new Error('rpc unreachable');
      },
    });

    const response = await fetch(`${base}/readyz`);

    expect(response.status).toBe(503);
    await expect(response.text()).resolves.not.toContain('rpc unreachable');
  });

  it('keeps healthz answering while unready so the platform can still reach it', async () => {
    const base = await serve({ readiness: async () => ({ feePayerLamports: 0 }) });

    await expect(fetch(`${base}/healthz`).then((r) => r.status)).resolves.toBe(200);
  });
});
