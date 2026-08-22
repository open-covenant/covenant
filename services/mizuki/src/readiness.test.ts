import { describe, expect, it, vi } from 'vitest';
import {
  ServiceReadiness,
  serviceDependencies,
  type ReadinessProbe,
  type ServiceDependency,
} from './readiness.js';

const refundProtection = {
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

describe('ServiceReadiness', () => {
  it('coalesces probes and reuses only bounded-fresh successful evidence', async () => {
    let now = 1_000;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const probes = healthyProbes();
    probes.postgres = vi.fn(async () => gate);
    const readiness = createReadiness(probes, () => now);

    const checks = [readiness.check(), readiness.check()];
    release();
    const [first] = await Promise.all(checks);
    expect(first).toMatchObject({
      ready: true,
      ageMs: 0,
      lastSuccessfulAgeMs: 0,
      dependencies: { policy_signer: { refundProtection } },
    });
    expect(readiness.latest()).toMatchObject({
      dependencies: { policy_signer: { refundProtection } },
    });
    expect(probes.postgres).toHaveBeenCalledTimes(1);

    await readiness.check();
    expect(probes.postgres).toHaveBeenCalledTimes(1);
    now += 101;
    await readiness.check();
    expect(probes.postgres).toHaveBeenCalledTimes(2);
  });

  it('identifies a failed dependency without exposing its error and recovers', async () => {
    let now = 1_000;
    let githubHealthy = false;
    const probes = healthyProbes();
    probes.github_app = vi.fn(async () => {
      if (!githubHealthy) throw new Error('secret upstream diagnostic');
    });
    const readiness = createReadiness(probes, () => now);

    const failed = await readiness.check();
    expect(failed).toMatchObject({
      ready: false,
      lastSuccessfulAt: null,
      failed: ['github_app', 'stale'],
    });
    expect(JSON.stringify(failed)).not.toContain('secret upstream');

    githubHealthy = true;
    now += 11;
    await expect(readiness.check()).resolves.toMatchObject({
      ready: true,
      failed: [],
      lastSuccessfulAgeMs: 0,
    });
  });

  it('fails closed when the last complete evidence becomes stale', async () => {
    let now = 1_000;
    let updaterHealthy = true;
    const probes = healthyProbes();
    probes.updater = vi.fn(async () => {
      if (!updaterHealthy) throw new Error('unavailable');
    });
    const readiness = createReadiness(probes, () => now);

    await expect(readiness.check()).resolves.toMatchObject({ ready: true });
    updaterHealthy = false;
    now += 301;
    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      lastSuccessfulAgeMs: 301,
      failed: ['updater', 'stale'],
    });
  });
});

function healthyProbes(): Record<ServiceDependency, ReturnType<typeof vi.fn<ReadinessProbe>>> {
  return Object.fromEntries(
    serviceDependencies.map((name) => [
      name,
      vi.fn(async () => (name === 'policy_signer' ? refundProtection : undefined)),
    ]),
  ) as Record<ServiceDependency, ReturnType<typeof vi.fn<ReadinessProbe>>>;
}

function createReadiness(
  probes: Record<ServiceDependency, ReadinessProbe>,
  now: () => number,
): ServiceReadiness {
  return new ServiceReadiness(probes, {
    refreshMs: 100,
    maxAgeMs: 300,
    timeoutMs: 20,
    failureRetryMs: 10,
    now,
  });
}
