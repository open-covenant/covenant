import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, ensureRefundCapacity, SerialGate, type AppDependencies } from './app.js';
import { loadConfig } from './config.js';
import { ServiceReadiness, serviceDependencies } from './readiness.js';
import { MemoryStore } from './store.js';

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
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1100000000',
      availableEscrowReserveLamports: '1000000000',
    };
    await expect(
      ensureRefundCapacity(
        {
          config,
          store,
          policy: { readiness: async () => readiness } as unknown as AppDependencies['policy'],
        },
        2_000_000n,
      ),
    ).resolves.toEqual(readiness);

    await expect(
      ensureRefundCapacity(
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
      ),
    ).rejects.toThrow('escrow capacity');
  });
});

describe('deployment readiness endpoint', () => {
  it('serves a strict authenticated functional readiness receipt', async () => {
    const token = 'd'.repeat(32);
    const base = await serve(
      { check: vi.fn(async () => ({ ready: true })) } as unknown as ServiceReadiness,
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
  });

  it('fails the functional receipt when any dependency is unready', async () => {
    const token = 'd'.repeat(32);
    const base = await serve(
      { check: vi.fn(async () => ({ ready: false })) } as unknown as ServiceReadiness,
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
): Promise<string> {
  const server = createServer(
    createApp({
      config: loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_RELEASE_PROBE_TOKEN: releaseProbeToken,
      }),
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
    intakeEnabled: true,
    claimsEnabled: true,
    reason: 'deployment readiness test controls',
    updatedBy: 'test',
  });
  return store;
}
