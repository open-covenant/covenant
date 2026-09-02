import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, SerialGate, type AppDependencies } from './app.js';
import { MemoryStore } from './store.js';
import type { Payment, Quote } from './types.js';

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map(
      (server) =>
        new Promise<void>((resolve, reject) => {
          server.close((cause) => (cause ? reject(cause) : resolve()));
        }),
    ),
  );
  vi.restoreAllMocks();
});

describe('public proof feed', () => {
  it('publishes settled work without requiring a session', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const { job } = await store.createJob(quote, payment, 'proof-settled');
    await store.transitionJob(job.id, 'settlement_pending', 'paid');
    const base = await serve(store);

    const response = await fetch(`${base}/v1/proof`);
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.count).toBe(1);
    expect(body.jobs[0].paymentTransaction).toBe('settlement');
    expect(body.jobs[0].issueUrl).toBe(quote.issueUrl);
    expect(body.settlement.network).toBe('solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp');
  });

  it('omits work that has not settled, so the feed is evidence rather than intent', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.createJob(quote, { ...payment, transaction: 'pending' }, 'proof-pending');
    const base = await serve(store);

    const body = await (await fetch(`${base}/v1/proof`)).json();

    expect(body.count).toBe(0);
    expect(body.jobs).toEqual([]);
  });

  it('never exposes anything beyond the public job shape', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const { job } = await store.createJob(quote, payment, 'proof-shape');
    await store.transitionJob(job.id, 'settlement_pending', 'paid');
    const base = await serve(store);

    const body = await (await fetch(`${base}/v1/proof`)).json();
    const serialized = JSON.stringify(body);

    // The stored job carries the issue body and the maintainer's idempotency key.
    // Neither belongs in a public feed.
    expect(serialized).not.toContain('proof-shape');
    expect(serialized).not.toContain('private-ish description');
    expect(serialized).not.toContain(payment.payer);
    // Pinned so a new field on the internal job cannot reach the feed unnoticed.
    expect(Object.keys(body.jobs[0]).sort()).toEqual([
      'changedFiles',
      'class',
      'costCoverage',
      'createdAt',
      'id',
      'issueTitle',
      'issueUrl',
      'paymentTransaction',
      'priceAtomic',
      'state',
      'updatedAt',
      'validations',
      'variableRouteCostEstimateUsd',
    ]);
  });

  it('bounds the page so one request cannot ask for the whole history', async () => {
    const store = new MemoryStore();
    for (let index = 0; index < 5; index += 1) {
      const scoped = { ...quote, id: `1111111${index}-1111-4111-8111-111111111111` };
      await store.saveQuote(scoped);
      const { job } = await store.createJob(scoped, payment, `proof-many-${index}`);
      await store.transitionJob(job.id, 'settlement_pending', 'paid');
    }
    const base = await serve(store);

    const limited = await (await fetch(`${base}/v1/proof?limit=2`)).json();
    expect(limited.count).toBe(2);

    // An absurd request is clamped rather than honoured.
    const clamped = await (await fetch(`${base}/v1/proof?limit=100000`)).json();
    expect(clamped.count).toBe(5);
  });
});

async function serve(store: MemoryStore): Promise<string> {
  const app = createApp(dependencies(store));
  const server = createServer((req, res) => void app(req, res));
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('server did not bind a port');
  return `http://127.0.0.1:${address.port}`;
}

function dependencies(store: MemoryStore): AppDependencies {
  return {
    config: {
      paymentMode: 'mock',
      webOrigin: 'https://mizuki.example',
      trustedProxyHops: 0,
      rateLimitMaxSources: 100,
      sseMaxConnections: 10,
      sseMaxConnectionsPerSource: 2,
      githubAuthorizationLabel: 'mizuki:authorized',
    },
    store,
    github: {},
    payments: {},
    processor: {},
    auth: { session: vi.fn(() => undefined), csrfToken: vi.fn(() => undefined) },
    gate: new SerialGate(),
  } as unknown as AppDependencies;
}

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl: 'https://github.com/example/project/issues/7',
  owner: 'example',
  repo: 'project',
  issueNumber: 7,
  issueTitle: 'Fix docs typo',
  issueBody: 'a private-ish description that should not be republished',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: ['npm test'],
  expiresAt: '2099-01-01T00:00:00.000Z',
};

const payment: Payment = {
  payer: '1'.repeat(32),
  transaction: 'settlement',
  amountAtomic: '2000000',
};
