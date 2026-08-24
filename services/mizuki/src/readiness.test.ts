import { describe, expect, it, vi } from 'vitest';
import {
  applicationDependencies,
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

  it('reports incomplete configuration without exposing secret values', async () => {
    const probes = healthyProbes();
    probes.configuration = vi.fn(async () => ({
      issues: ['MIZUKI_POLICY_SIGNER_TOKEN', 'USEPOD_API_KEY'],
    }));
    const readiness = createReadiness(probes, () => 1_000);

    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      failed: ['configuration', 'stale'],
      dependencies: {
        configuration: {
          ok: false,
          configurationIssues: ['MIZUKI_POLICY_SIGNER_TOKEN', 'USEPOD_API_KEY'],
        },
      },
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

  it('keeps application readiness independent while updater remains operator-critical', async () => {
    const probes = healthyProbes();
    probes.updater = vi.fn(async () => {
      throw new Error('recursive controller dependency');
    });
    const readiness = createReadiness(probes, () => 1_000);

    await expect(readiness.checkApplication()).resolves.toMatchObject({
      ready: true,
      failed: [],
    });
    expect(probes.updater).not.toHaveBeenCalled();

    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      failed: ['updater', 'stale'],
    });
    expect(probes.updater).toHaveBeenCalledTimes(1);
    await expect(readiness.checkApplication()).resolves.toMatchObject({ ready: true, failed: [] });
    expect(probes.updater).toHaveBeenCalledTimes(1);
  });

  it('terminates when updater readiness re-enters application readiness', async () => {
    const probes = healthyProbes();
    let readiness!: ServiceReadiness;
    probes.updater = vi.fn(async () => {
      const report = await readiness.checkApplication();
      if (!report.ready) throw new Error('application is not ready');
    });
    readiness = createReadiness(probes, () => 1_000);

    await expect(readiness.check()).resolves.toMatchObject({ ready: true, failed: [] });
    expect(probes.updater).toHaveBeenCalledTimes(1);
    for (const name of applicationDependencies) {
      expect(probes[name]).toHaveBeenCalled();
    }
  });

  it.each(applicationDependencies)('fails application readiness when %s fails', async (name) => {
    const probes = healthyProbes();
    probes[name] = vi.fn(async () => {
      throw new Error('dependency unavailable');
    });
    const readiness = createReadiness(probes, () => 1_000);

    await expect(readiness.checkApplication()).resolves.toMatchObject({
      ready: false,
      failed: [name, 'stale'],
    });
    expect(probes.updater).not.toHaveBeenCalled();
  });

  it('coalesces concurrent application probes independently from operator evidence', async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const probes = healthyProbes();
    probes.postgres = vi.fn(async () => gate);
    const readiness = createReadiness(probes, () => 1_000);

    const checks = [readiness.checkApplication(), readiness.checkApplication()];
    release();
    await expect(Promise.all(checks)).resolves.toHaveLength(2);
    expect(probes.postgres).toHaveBeenCalledTimes(1);
    expect(probes.updater).not.toHaveBeenCalled();
    expect(readiness.latest()).toBeUndefined();
  });
});

function healthyProbes(): Record<ServiceDependency, ReturnType<typeof vi.fn<ReadinessProbe>>> {
  return Object.fromEntries(
    serviceDependencies.map((name) => [
      name,
      vi.fn(async () => {
        if (name === 'configuration') return { issues: [] };
        return name === 'policy_signer' ? refundProtection : undefined;
      }),
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
