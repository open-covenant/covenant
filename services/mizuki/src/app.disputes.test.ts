import { createServer } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, type AppDependencies } from './app.js';
import type { RescueBounty } from './domain/index.js';

const servers: ReturnType<typeof createServer>[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map(
      (server) =>
        new Promise<void>((resolve, reject) => {
          server.close((cause) => (cause ? reject(cause) : resolve()));
        }),
    ),
  );
});

describe('dispute resolution API', () => {
  it('requires admin authentication and forwards evidence with an idempotency key', async () => {
    const resolved = resolvedBounty();
    const resolveDispute = vi.fn(async () => resolved);
    const app = createApp({
      config: { adminToken: 'admin-secret' },
      store: {
        job: vi.fn(async () => undefined),
        escrowByBounty: vi.fn(async () => undefined),
        contributor: vi.fn(async () => undefined),
      },
      bounties: { resolveDispute },
    } as unknown as AppDependencies);
    const server = createServer(app);
    servers.push(server);
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('test server did not bind');
    const url = `http://127.0.0.1:${address.port}/v1/admin/bounties/bounty-1/disputes/dispute-1/resolve`;
    const body = JSON.stringify({
      decision: 'refund',
      evidence: {
        summary: 'The issue evidence supports returning the bounty escrow to treasury.',
        references: ['https://github.com/example/project/issues/1'],
      },
    });

    const unauthorized = await fetch(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'resolve-1' },
      body,
    });
    expect(unauthorized.status).toBe(401);
    expect(resolveDispute).not.toHaveBeenCalled();

    const response = await fetch(url, {
      method: 'POST',
      headers: {
        authorization: 'Bearer admin-secret',
        'content-type': 'application/json',
        'idempotency-key': 'resolve-1',
      },
      body,
    });
    expect(response.status).toBe(200);
    expect(resolveDispute).toHaveBeenCalledWith('bounty-1', 'dispute-1', {
      decision: 'refund',
      evidence: {
        summary: 'The issue evidence supports returning the bounty escrow to treasury.',
        references: ['https://github.com/example/project/issues/1'],
      },
      idempotencyKey: 'resolve-1',
    });
    await expect(response.json()).resolves.toMatchObject({
      bounty: { id: 'bounty-1', state: 'refunded' },
      dispute: { id: 'dispute-1', state: 'refunded' },
    });
  });
});

function resolvedBounty(): RescueBounty {
  return {
    id: 'bounty-1',
    sourceJobId: 'job-1',
    failureReceiptId: 'failure-1',
    repository: 'example/project',
    issueNumber: 1,
    issueUrl: 'https://github.com/example/project/issues/1',
    priceCents: 1_000,
    generation: 0,
    offerExpiresAt: '2026-08-29T10:00:00.000Z',
    state: 'refunded',
    activeClaim: {
      id: 'claim-1',
      claimantId: 'contributor-1',
      walletAddress: 'wallet-1',
      state: 'refunded',
      claimedAt: '2026-08-22T10:00:00.000Z',
      leaseExpiresAt: '2026-08-24T10:00:00.000Z',
      closedAt: '2026-08-24T10:01:00.000Z',
    },
    dispute: {
      id: 'dispute-1',
      claimantId: 'contributor-1',
      reason: 'The requested patch cannot be completed safely within scope.',
      state: 'refunded',
      openedAt: '2026-08-22T11:00:00.000Z',
      resolution: {
        id: 'resolution-1',
        idempotencyKey: 'resolve-1',
        requestedDecision: 'refund',
        settlementDecision: 'refund',
        evidence: {
          summary: 'The issue evidence supports returning the bounty escrow to treasury.',
          references: ['https://github.com/example/project/issues/1'],
        },
        evidenceHash: 'a'.repeat(64),
        decidedAt: '2026-08-22T12:00:00.000Z',
        resolvedAt: '2026-08-24T10:01:00.000Z',
        transactionSignature: 'refund-transaction',
      },
    },
    claimHistory: [],
    createdAt: '2026-08-22T09:00:00.000Z',
    updatedAt: '2026-08-24T10:01:00.000Z',
    revision: 8,
  };
}
