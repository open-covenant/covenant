import type { AddressInfo } from 'node:net';
import { afterEach, describe, expect, it } from 'vitest';
import type { Gate } from './gate.js';
import { createFacilitatorServer, type FacilitatorApi } from './server.js';
import { Store } from './store.js';

const servers: { close(): void }[] = [];
afterEach(() => servers.splice(0).forEach((server) => server.close()));

async function start(overrides: Partial<FacilitatorApi> = {}) {
  const store = new Store(':memory:');
  const gate = {
    verify: async () => ({ isValid: true, payer: 'payer' }),
    settle: async () => ({
      response: { success: true, transaction: '0xabc', network: 'eip155:8453', payer: 'payer' },
      bazaar: { status: 'success' as const },
    }),
  } as unknown as Gate;
  const api: FacilitatorApi = {
    gate,
    store,
    supported: () => ({ kinds: [{ x402Version: 2, scheme: 'exact', network: 'eip155:8453' }], extensions: ['bazaar'] }),
    about: () => ({ name: 'Covenant x402 facilitator' }),
    readiness: async () => ({ ok: true, networks: {} }),
    ...overrides,
  };
  const server = createFacilitatorServer(api, { maxRequestBytes: 4_096 }, () => {});
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  return { url: `http://127.0.0.1:${port}`, store };
}

const payment = { paymentPayload: { x402Version: 2 }, paymentRequirements: { network: 'eip155:8453' } };

describe('facilitator HTTP', () => {
  it('serves /supported to anyone, with CORS for browsers', async () => {
    const { url } = await start();
    const response = await fetch(`${url}/supported`);
    expect(response.status).toBe(200);
    expect(response.headers.get('access-control-allow-origin')).toBe('*');
    expect(await response.json()).toMatchObject({ extensions: ['bazaar'] });
  });

  it('returns the Bazaar outcome to the seller in EXTENSION-RESPONSES, not in the body', async () => {
    const { url } = await start();
    const response = await fetch(`${url}/settle`, { method: 'POST', body: JSON.stringify(payment) });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ success: true, transaction: '0xabc', network: 'eip155:8453', payer: 'payer' });
    const header = response.headers.get('extension-responses');
    expect(JSON.parse(Buffer.from(header!, 'base64').toString('utf8'))).toEqual({ bazaar: { status: 'success' } });
  });

  it.each([
    ['no body', undefined, 400],
    ['a body that is not JSON', 'nope', 400],
    ['a missing paymentRequirements', JSON.stringify({ paymentPayload: {} }), 400],
    ['a body over the limit', JSON.stringify({ ...payment, pad: 'x'.repeat(5_000) }), 413],
  ])('refuses %s', async (_name, body, status) => {
    const { url } = await start();
    const response = await fetch(`${url}/verify`, { method: 'POST', body });
    expect(response.status).toBe(status);
  });

  it('pages /discovery/resources in the shape the x402 client expects', async () => {
    const { url, store } = await start();
    for (const n of [1, 2, 3]) {
      store.upsertCatalogItem(
        {
          resource: `https://seller.example/${n}`,
          type: 'http',
          x402Version: 2,
          accepts: [{ scheme: 'exact', network: 'eip155:8453', payTo: '0xseller' }],
          lastUpdated: new Date(n * 1000).toISOString(),
          description: n === 2 ? 'Weather forecasts' : 'Text inference',
        },
        'POST',
        n,
      );
    }
    const page = await (await fetch(`${url}/discovery/resources?limit=2&offset=1`)).json();
    expect(page).toMatchObject({ x402Version: 2, pagination: { limit: 2, offset: 1, total: 3 } });
    expect(page.items.map((item: { resource: string }) => item.resource)).toEqual([
      'https://seller.example/2',
      'https://seller.example/1',
    ]);
    const found = await (await fetch(`${url}/discovery/search?query=weather`)).json();
    expect(found.resources.map((item: { resource: string }) => item.resource)).toEqual(['https://seller.example/2']);
    expect((await fetch(`${url}/discovery/resources?limit=101`)).status).toBe(400);
    expect((await fetch(`${url}/discovery/search`)).status).toBe(400);
  });

  it('reports unready when a signer is short of gas', async () => {
    const { url } = await start({ readiness: async () => ({ ok: false, networks: { 'eip155:8453': { ok: false } } }) });
    expect((await fetch(`${url}/readyz`)).status).toBe(503);
    expect((await fetch(`${url}/healthz`)).status).toBe(200);
  });

  it('answers 404 for anything else', async () => {
    const { url } = await start();
    expect((await fetch(`${url}/admin`)).status).toBe(404);
    expect((await fetch(`${url}/settle`)).status).toBe(404);
  });

  it('limits how often one client can call /verify and /settle', async () => {
    const { url } = await start();
    const statuses: number[] = [];
    for (let i = 0; i < 121; i++) {
      const response = await fetch(`${url}/verify`, {
        method: 'POST',
        body: JSON.stringify(payment),
        headers: { 'cf-connecting-ip': '203.0.113.7' },
      });
      statuses.push(response.status);
    }
    expect(statuses.filter((status) => status === 429)).toHaveLength(1);
    const other = await fetch(`${url}/verify`, {
      method: 'POST',
      body: JSON.stringify(payment),
      headers: { 'cf-connecting-ip': '203.0.113.8' },
    });
    expect(other.status).toBe(200);
  });
});
