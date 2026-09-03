import { randomUUID } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it } from 'vitest';
import { createApp, SerialGate, type AppDependencies } from './app.js';
import { loadConfig } from './config.js';
import type { ServiceReadinessReport } from './readiness.js';
import type { SocialBrief } from './social.js';
import { MemoryStore } from './store.js';
import type { Quote } from './types.js';

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
});

describe('social API', () => {
  it('serves a public fact pack and records a validated confirmed post', async () => {
    const store = new MemoryStore();
    const value = quote();
    const { job } = await store.createJob(
      value,
      { payer: 'payer', transaction: randomUUID(), amountAtomic: value.priceAtomic },
      randomUUID(),
    );
    await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      prUrl: 'https://github.com/internal/tool/pull/1',
      mergedAt: '2026-08-25T16:00:00.000Z',
      refundLiabilityId: randomUUID(),
      refundLiabilityDischargedAt: '2026-08-25T16:00:00.000Z',
    });
    const base = await serve(store);

    const response = await fetch(`${base}/v1/social/brief?kind=stats`);
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    const brief = (await response.json()) as SocialBrief;
    expect(brief).toMatchObject({
      publishable: true,
      metrics: {
        internalPaidAttempts: { total: 1, delta: 1 },
        externalPaidJobs: { total: 0, delta: 0 },
      },
    });

    const prUrl = brief.evidence.find(({ claim }) => claim === 'mergedPr')?.url;
    const text = `Internal test: 1 operator-funded attempt and 1 merged PR. ${prUrl}`;
    const validated = await fetch(`${base}/v1/social/validate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        cursor: brief.cursor,
        sourceHash: brief.sourceHash,
        output: `POST\n${text}`,
      }),
    });
    expect(validated.status).toBe(200);
    await expect(validated.json()).resolves.toEqual({ valid: true, decision: 'post', text });

    const recorded = await fetch(`${base}/v1/admin/social/posts`, {
      method: 'POST',
      headers: {
        authorization: 'Bearer admin-secret',
        'content-type': 'application/json',
      },
      body: JSON.stringify({
        kind: brief.kind,
        cursor: brief.cursor,
        sourceHash: brief.sourceHash,
        postId: '1234567890123456789',
        text,
      }),
    });
    expect(recorded.status).toBe(201);
    await expect(recorded.json()).resolves.toMatchObject({
      postId: '1234567890123456789',
      text,
    });

    const repeated = await fetch(`${base}/v1/social/brief?kind=stats`);
    await expect(repeated.json()).resolves.toMatchObject({
      publishable: false,
      blockedReasons: expect.arrayContaining(['duplicate_source', 'no_changes_since_last_post']),
      metrics: { internalPaidAttempts: { total: 1, delta: 0 } },
    });
  });

  it('rejects unsupported brief kinds and unauthenticated receipt reads', async () => {
    const base = await serve(new MemoryStore());

    expect((await fetch(`${base}/v1/social/brief?kind=product_update`)).status).toBe(400);
    expect((await fetch(`${base}/v1/admin/social/posts`)).status).toBe(401);
    const invalid = await fetch(`${base}/v1/social/validate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({}),
    });
    expect(invalid.status).toBe(400);
  });
});

async function serve(store: MemoryStore): Promise<string> {
  const report = readiness();
  const app = createApp({
    config: loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_INTERNAL_REPOS: 'internal/tool',
      MIZUKI_PUBLIC_BASE_URL: 'https://api.example.test',
      MIZUKI_WEB_ORIGIN: 'https://mizuki.example.test',
      MIZUKI_ADMIN_TOKEN: 'admin-secret',
    }),
    store,
    paymentAdmission: new SerialGate(),
    readiness: { check: async () => report },
  } as unknown as AppDependencies);
  const server = createServer(app);
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('test server did not bind');
  return `http://127.0.0.1:${address.port}`;
}

function quote(): Quote {
  return {
    id: randomUUID(),
    issueUrl: 'https://github.com/internal/tool/issues/1',
    owner: 'internal',
    repo: 'tool',
    issueNumber: 1,
    issueTitle: 'Fix bounded issue',
    issueBody: '',
    baseSha: 'a'.repeat(40),
    defaultBranch: 'main',
    class: 'micro',
    priceAtomic: '2000000',
    maxFiles: 3,
    maxCostUsd: 0.8,
    validationCommands: [],
    expiresAt: '2099-01-01T00:00:00.000Z',
  };
}

function readiness(): ServiceReadinessReport {
  const checkedAt = '2026-08-25T16:00:00.000Z';
  return {
    ready: true,
    checkedAt,
    ageMs: 0,
    lastSuccessfulAt: checkedAt,
    lastSuccessfulAgeMs: 0,
    failed: [],
    dependencies: {
      policy_signer: {
        ok: true,
        checkedAt,
        latencyMs: 1,
        refundProtection: {
          refundTreasury: 'refund-treasury',
          refundMint: 'usdc-mint',
          refundDecimals: 6,
          finalizedBalanceRaw: '100000000',
          pendingRefundRaw: '0',
          treasuryAvailableRefundRaw: '100000000',
          remainingRefundLimitUsdCents: 10_000,
          availableRefundRaw: '100000000',
          escrowAuthority: 'escrow-authority',
          finalizedEscrowBalanceLamports: '2000000000',
          availableEscrowReserveLamports: '1900000000',
        },
      },
    },
  } as ServiceReadinessReport;
}
