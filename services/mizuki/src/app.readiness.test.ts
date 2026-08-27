import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, ensurePaymentCapacity, SerialGate, type AppDependencies } from './app.js';
import { loadConfig } from './config.js';
import { UsePodContributorReviewer } from './contributor-reviewer.js';
import type { ContributorEscrow, RescueBounty } from './domain/index.js';
import type { GithubClient } from './github.js';
import { ServiceReadiness, serviceDependencies } from './readiness.js';
import { MemoryStore, type MizukiStore } from './store.js';
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

describe('service readiness endpoint', () => {
  it('returns non-secret dependency evidence and fails closed', async () => {
    const probes = Object.fromEntries(
      serviceDependencies.map((name) => [
        name,
        vi.fn(async () => {
          if (name === 'configuration') return { issues: [] };
          if (name === 'github_app') throw new Error('secret upstream diagnostic');
          if (name === 'policy_signer') {
            return {
              refundTreasury: 'refund-treasury',
              refundMint: 'usdc-mint',
              refundDecimals: 6,
              finalizedBalanceRaw: '12000000',
              pendingRefundRaw: '1000000',
              treasuryAvailableRefundRaw: '11000000',
              remainingRefundLimitUsdCents: 1_100,
              availableRefundRaw: '11000000',
              escrowAuthority: 'escrow-authority',
              finalizedEscrowBalanceLamports: '2000000000',
              availableEscrowReserveLamports: '1900000000',
            };
          }
        }),
      ]),
    ) as ConstructorParameters<typeof ServiceReadiness>[0];
    const readiness = new ServiceReadiness(probes, {
      refreshMs: 100,
      maxAgeMs: 300,
      timeoutMs: 20,
    });
    const base = await serve(readiness);

    const health = await fetch(`${base}/healthz`);
    expect(health.status).toBe(200);
    await expect(health.json()).resolves.toEqual({ ok: true });

    const response = await fetch(`${base}/readyz`);
    expect(response.status).toBe(503);
    expect(response.headers.get('cache-control')).toBe('no-store');
    const body = await response.json();
    expect(body).toMatchObject({
      ready: false,
      failed: expect.arrayContaining(['github_app']),
      dependencies: { github_app: { ok: false } },
    });
    expect(body.dependencies.policy_signer).toMatchObject({
      ok: true,
      refundProtection: {
        refundTreasury: 'refund-treasury',
        refundMint: 'usdc-mint',
        refundDecimals: 6,
        finalizedBalanceRaw: '12000000',
        pendingRefundRaw: '1000000',
        availableRefundRaw: '11000000',
        escrowAuthority: 'escrow-authority',
        finalizedEscrowBalanceLamports: '2000000000',
        availableEscrowReserveLamports: '1900000000',
      },
    });
    expect(JSON.stringify(body)).not.toContain('secret upstream');

    const treasuryResponse = await fetch(`${base}/v1/treasury`);
    expect(treasuryResponse.status).toBe(200);
    await expect(treasuryResponse.json()).resolves.toMatchObject({
      refundProtection: {
        status: 'unavailable',
        finalizedBalanceAtomic: null,
      },
      allocationModel: { custodyVerified: false },
    });
  });

  it('binds public custody fields to a fresh complete readiness report', async () => {
    const probes = Object.fromEntries(
      serviceDependencies.map((name) => [
        name,
        vi.fn(async () => {
          if (name === 'configuration') return { issues: [] };
          if (name !== 'policy_signer') return;
          return {
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
          };
        }),
      ]),
    ) as ConstructorParameters<typeof ServiceReadiness>[0];
    const readiness = new ServiceReadiness(probes, {
      refreshMs: 100,
      maxAgeMs: 300,
      timeoutMs: 20,
    });
    const base = await serve(readiness);

    const response = await fetch(`${base}/v1/treasury`);
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      refundProtection: {
        status: 'verified',
        source: 'policy_signer_finalized',
        refundTreasury: 'refund-treasury',
        refundMint: 'usdc-mint',
        refundDecimals: 6,
        finalizedBalanceAtomic: '100000000',
        signerOutstandingLiabilityAtomic: '0',
        unencumberedBalanceAtomic: '100000000',
        newIntakeCapacityAtomic: '100000000',
        liabilityReconciled: true,
        liabilitiesBacked: true,
      },
      allocationModel: {
        source: 'application_ledger',
        custodyVerified: false,
      },
    });
  });

  it('keeps public readiness reads non-billable, cached, and fail-closed', async () => {
    const request = vi.fn<typeof fetch>(async (input, init) => {
      expect(String(input)).toBe('https://api.usepod.ai/proxy/secret/v1/models');
      expect(init?.method).toBe('GET');
      expect(init?.body).toBeUndefined();
      const headers = new Headers(init?.headers);
      expect(headers.get('content-type')).toBeNull();
      expect(headers.get('x-pod-routing-mode')).toBeNull();
      expect(headers.get('x-pod-max-price-input')).toBeNull();
      expect(headers.get('x-pod-max-price-output')).toBeNull();
      return Response.json({ object: 'list', data: [{ id: 'unavailable-model' }] });
    });
    const reviewer = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'secret',
        USEPOD_REVIEW_MODEL: 'independent-reviewer',
      }),
      {} as MizukiStore,
      {} as GithubClient,
      request,
    );
    const probes = Object.fromEntries(
      serviceDependencies.map((name) => [
        name,
        vi.fn(async () => {
          if (name === 'configuration') return { issues: [] };
          if (name === 'reviewer_route') return reviewer.readiness();
          if (name !== 'policy_signer') return;
          return {
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
          };
        }),
      ]),
    ) as ConstructorParameters<typeof ServiceReadiness>[0];
    const readiness = new ServiceReadiness(probes, {
      refreshMs: 60_000,
      maxAgeMs: 120_000,
      timeoutMs: 1_000,
      failureRetryMs: 60_000,
    });
    const base = await serve(readiness);

    const ready = await fetch(`${base}/readyz`);
    expect(ready.status).toBe(503);
    await expect(ready.json()).resolves.toMatchObject({
      ready: false,
      dependencies: { reviewer_route: { ok: false } },
    });

    const publicMetrics = await fetch(`${base}/v1/metrics`);
    expect(publicMetrics.status).toBe(200);
    await expect(publicMetrics.json()).resolves.toMatchObject({
      refundProtection: { status: 'unavailable' },
    });

    const prometheusMetrics = await fetch(`${base}/metrics`);
    expect(prometheusMetrics.status).toBe(200);
    expect(await prometheusMetrics.text()).toContain('mizuki_refund_protection_verified 0');

    const treasury = await fetch(`${base}/v1/treasury`);
    expect(treasury.status).toBe(200);
    await expect(treasury.json()).resolves.toMatchObject({
      refundProtection: { status: 'unavailable' },
    });

    expect(request).toHaveBeenCalledTimes(1);
  });

  it('requires signer-reported refund and rescue escrow capacity', async () => {
    const config = loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_PAY_TO: 'refund-treasury',
      CLAWPUMP_PAYOUT_WALLET: 'escrow-authority',
      MIZUKI_ESCROW_READINESS_MIN_LAMPORTS: '1000000000',
    });
    const store = new MemoryStore();
    const readiness = {
      healthy: true,
      refundTreasury: 'refund-treasury',
      refundMint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
      refundDecimals: 6,
      finalizedBalanceRaw: '12000000',
      pendingRefundRaw: '1000000',
      treasuryAvailableRefundRaw: '11000000',
      remainingRefundLimitUsdCents: 1_100,
      availableRefundRaw: '11000000',
      availableRefundTransactions: 10,
      remainingEscrowLimitUsdCents: 1_000,
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1100000000',
      availableEscrowReserveLamports: '1000000000',
    };
    await expect(
      ensurePaymentCapacity(
        {
          config,
          store,
          policy: { readiness: async () => readiness } as unknown as AppDependencies['policy'],
        },
        2_000_000n,
        1_000,
      ),
    ).resolves.toEqual(readiness);

    await expect(
      ensurePaymentCapacity(
        {
          config,
          store,
          policy: {
            readiness: async () => ({
              ...readiness,
              availableEscrowReserveLamports: '999999999',
            }),
          } as unknown as AppDependencies['policy'],
        },
        2_000_000n,
        1_000,
      ),
    ).rejects.toThrow('escrow capacity');

    await expect(
      ensurePaymentCapacity(
        {
          config,
          store,
          policy: {
            readiness: async () => ({
              ...readiness,
              remainingEscrowLimitUsdCents: 999,
            }),
          } as unknown as AppDependencies['policy'],
        },
        2_000_000n,
        1_000,
      ),
    ).rejects.toThrow('rolling escrow capacity');
  });

  it('reserves successor capacity while an expired claim escrow refund is pending', async () => {
    const config = loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_PAY_TO: 'refund-treasury',
      CLAWPUMP_PAYOUT_WALLET: 'escrow-authority',
      MIZUKI_ESCROW_READINESS_MIN_LAMPORTS: '1000000000',
    });
    const store = new MemoryStore();
    const quote: Quote = {
      id: '11111111-1111-4111-8111-111111111111',
      issueUrl: 'https://github.com/example/project/issues/1',
      owner: 'example',
      repo: 'project',
      issueNumber: 1,
      issueTitle: 'Fix docs',
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
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment', amountAtomic: quote.priceAtomic },
      'paid-job',
    );
    await store.transitionJob(job.id, 'settlement_pending', 'refunded', {
      refundTransaction: 'refund',
    });
    const bounty: RescueBounty = {
      id: '22222222-2222-4222-8222-222222222222',
      sourceJobId: job.id,
      failureReceiptId: `failure:${job.id}`,
      repository: 'example/project',
      issueNumber: 1,
      issueUrl: quote.issueUrl,
      priceCents: 1_000,
      generation: 0,
      offerExpiresAt: '2026-08-30T00:00:00.000Z',
      state: 'claim_refund_pending',
      activeClaim: {
        id: 'claim-1',
        claimantId: 'github:1',
        walletAddress: 'claimant-wallet',
        state: 'expired',
        claimedAt: '2026-08-20T00:00:00.000Z',
        leaseExpiresAt: '2026-08-22T00:00:00.000Z',
        closedAt: '2026-08-22T00:00:00.000Z',
      },
      claimHistory: [],
      createdAt: '2026-08-20T00:00:00.000Z',
      updatedAt: '2026-08-22T00:00:00.000Z',
      revision: 5,
    };
    await store.createBounty(bounty);
    await store.saveEscrow({
      id: '33333333-3333-4333-8333-333333333333',
      bountyId: bounty.id,
      repository: bounty.repository,
      issueNumber: bounty.issueNumber,
      issueTitle: quote.issueTitle,
      issueBody: quote.issueBody,
      baseRef: quote.defaultBranch,
      baseSha: quote.baseSha,
      reviewPolicy: { version: 1, model: 'reviewer', maxFiles: quote.maxFiles },
      amountCents: bounty.priceCents,
      acceptanceHash: 'b'.repeat(64),
      expiresAt: bounty.offerExpiresAt,
      state: 'refund_pending',
      reservationId: 'historic-reservation',
      amountAtomic: '100000000',
      fundingSignature: 'historic-funding',
      createdAt: bounty.createdAt,
      updatedAt: bounty.updatedAt,
      revision: 0,
    } satisfies ContributorEscrow);
    const readiness = {
      healthy: true,
      refundTreasury: 'refund-treasury',
      refundMint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
      refundDecimals: 6,
      finalizedBalanceRaw: '12000000',
      pendingRefundRaw: '0',
      treasuryAvailableRefundRaw: '12000000',
      remainingRefundLimitUsdCents: 1_200,
      availableRefundRaw: '12000000',
      availableRefundTransactions: 10,
      remainingEscrowLimitUsdCents: 1_999,
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1100000000',
      availableEscrowReserveLamports: '1000000000',
    };
    const order: string[] = [];
    const bountyBySourceJob = store.bountyBySourceJob.bind(store);
    vi.spyOn(store, 'bountyBySourceJob').mockImplementation(async (jobId) => {
      order.push('application');
      return bountyBySourceJob(jobId);
    });

    await expect(
      ensurePaymentCapacity(
        {
          config,
          store,
          policy: {
            readiness: async () => {
              order.push('signer');
              return readiness;
            },
          } as unknown as AppDependencies['policy'],
        },
        2_000_000n,
        1_000,
      ),
    ).rejects.toThrow('rolling escrow capacity');
    expect(order).toEqual(['application', 'signer']);
  });
});

describe('deployment readiness endpoint', () => {
  it('serves a strict authenticated functional readiness receipt', async () => {
    const token = 'd'.repeat(32);
    const check = vi.fn(async () => ({ ready: false }));
    const checkApplication = vi.fn(async () => ({ ready: true }));
    const base = await serve(
      { check, checkApplication } as unknown as ServiceReadiness,
      new MemoryStore(),
      token,
    );

    expect((await fetch(`${base}/internal/mizuki/functional-readiness`)).status).toBe(401);
    const response = await fetch(`${base}/internal/mizuki/functional-readiness`, {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    await expect(response.json()).resolves.toEqual({
      status: 'ok',
      service: 'mizuki-api',
      checks: {
        database: 'ok',
        policySigner: 'ok',
        codingGateway: 'ok',
        settlement: 'ok',
      },
    });
    expect(checkApplication).toHaveBeenCalledTimes(1);
    expect(check).not.toHaveBeenCalled();
  });

  it('fails the functional receipt when any dependency is unready', async () => {
    const token = 'd'.repeat(32);
    const base = await serve(
      { checkApplication: vi.fn(async () => ({ ready: false })) } as unknown as ServiceReadiness,
      new MemoryStore(),
      token,
    );
    const response = await fetch(`${base}/internal/mizuki/functional-readiness`, {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ status: 'unavailable' });
  });

  it('passes with closed durable controls while dependencies are unready', async () => {
    const check = vi.fn(async () => ({ ready: false }));
    const base = await serve({ check } as unknown as ServiceReadiness);

    const response = await fetch(`${base}/deployz`);

    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    await expect(response.json()).resolves.toEqual({ ok: true });
    expect(check).not.toHaveBeenCalled();
  });

  it('fails when durable controls are open and dependencies are unready', async () => {
    const store = await openControls();
    const base = await serve(
      { check: vi.fn(async () => ({ ready: false })) } as unknown as ServiceReadiness,
      store,
    );

    const response = await fetch(`${base}/deployz`);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ ok: false });
  });

  it('passes when durable controls are open and dependencies are ready', async () => {
    const store = await openControls();
    const base = await serve(
      { check: vi.fn(async () => ({ ready: true })) } as unknown as ServiceReadiness,
      store,
    );

    const response = await fetch(`${base}/deployz`);

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({ ok: true });
  });

  it('requires closed durable controls for a shadow deployment', async () => {
    const check = vi.fn(async () => ({ ready: true }));
    const base = await serve(
      { check } as unknown as ServiceReadiness,
      await openControls(),
      undefined,
      'shadow',
    );

    const response = await fetch(`${base}/deployz`);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ ok: false });
    expect(check).not.toHaveBeenCalled();
  });

  it('fails when durable controls cannot be read', async () => {
    const store = {
      operatorControls: vi.fn(async () => {
        throw new Error('database unavailable');
      }),
    } as unknown as AppDependencies['store'];
    const check = vi.fn(async () => ({ ready: true }));
    const base = await serve({ check } as unknown as ServiceReadiness, store);

    const response = await fetch(`${base}/deployz`);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ ok: false });
    expect(check).not.toHaveBeenCalled();
  });
});

async function serve(
  readiness: ServiceReadiness,
  store: AppDependencies['store'] = new MemoryStore(),
  releaseProbeToken?: string,
  runtimeRole: 'production' | 'shadow' = 'production',
): Promise<string> {
  const env = {
    MIZUKI_PAYMENT_MODE: 'mock',
    ...(releaseProbeToken ? { MIZUKI_RELEASE_PROBE_TOKEN: releaseProbeToken } : {}),
    ...(runtimeRole === 'shadow'
      ? { MIZUKI_RUNTIME_ROLE: 'shadow', MIZUKI_REQUIRE_GITHUB_APP: '0' }
      : {}),
  };
  const server = createServer(
    createApp({
      config: loadConfig(env),
      store,
      paymentAdmission: new SerialGate(),
      readiness,
    } as unknown as AppDependencies),
  );
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('test server did not bind');
  return `http://127.0.0.1:${address.port}`;
}

async function openControls(): Promise<MemoryStore> {
  const store = new MemoryStore();
  await store.updateOperatorControls({
    expectedRevision: 0,
    intakeEnabled: true,
    claimsEnabled: true,
    reason: 'deployment readiness test controls',
    updatedBy: 'test',
  });
  return store;
}
