import { describe, expect, it } from 'vitest';
import { capabilityHandoff } from './capability-handoff.js';
import { CapabilityService } from './capabilities.js';
import { MemoryStore } from './store.js';
import type { Job } from './types.js';
import type { ObservedUpgrade, UpgradeStatusReader } from './updater-client.js';

describe('CapabilityService', () => {
  it('proposes an upgrade after a failed standard job', async () => {
    const store = new MemoryStore();
    const service = new CapabilityService(store, undefined, () => new Date('2026-08-22T12:00:00Z'));
    const upgrade = await service.recordFailure(job('standard', 'UsePod route timed out'));
    expect(upgrade?.triggerReasons).toContain('standard_job_failure');
    expect(await store.capabilitiesList()).toMatchObject([
      { key: 'model.route-reliability', state: 'proposed' },
    ]);
  });

  it('publishes the first refunded micro failure and reuses its active proposal', async () => {
    const store = new MemoryStore();
    let day = 22;
    const service = new CapabilityService(
      store,
      undefined,
      () => new Date(`2026-08-${String(day++).padStart(2, '0')}T12:00:00Z`),
    );
    const first = await service.recordFailure(job('micro', 'repository validation failed', 'a'));
    const repeated = await service.recordFailure(job('micro', 'repository validation failed', 'b'));
    expect(first?.triggerReasons).toContain('paid_job_failure');
    expect(repeated?.id).toBe(first?.id);
  });

  it('activates only after the updater reports a completed signed proposal', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'route failed'));
    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'completed');

    const first = await service.reconcileUpdater();
    expect(first).toMatchObject({ observed: 1, failed: 0 });
    expect((await store.upgradesList())[0]).toMatchObject({
      state: 'active',
      evidence: {
        updaterState: 'completed',
        sourceHandoffHash: updater.observed.sourceHandoffSha256,
        benchmarkReceiptId: 'benchmark-1',
        reviewReceiptId: 'review-1',
        manifestHash: 'a'.repeat(64),
        deploymentId: 'deployment-1',
        promotionOperationId: 'promotion-1',
        promotionHealthyAt: '2026-08-22T12:00:00.000Z',
      },
    });
    expect((await store.capabilitiesList())[0]).toMatchObject({
      state: 'active',
      activeUpgradeId: proposal!.id,
    });
    expect(
      (await store.activity()).filter((event) => event.kind === 'capability.activated'),
    ).toHaveLength(1);

    const revision = (await store.upgradesList())[0].revision;
    await service.reconcileUpdater();
    expect((await store.upgradesList())[0].revision).toBe(revision);
    expect(
      (await store.activity()).filter((event) => event.kind === 'capability.activated'),
    ).toHaveLength(1);
  });

  it('does not activate while the promoted candidate is still in its soak window', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'route failed'));
    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'verifying_promotion');

    expect(await service.reconcileUpdater()).toMatchObject({ observed: 1, failed: 0 });
    expect((await store.upgradesList())[0]).toMatchObject({
      state: 'staging',
      evidence: { updaterState: 'verifying_promotion' },
    });
    expect((await store.capabilitiesList())[0].state).toBe('validating');
  });

  it('does not invent progress before an external proposal is submitted', async () => {
    const store = new MemoryStore();
    const service = new CapabilityService(
      store,
      {
        async getByProposalId() {
          return undefined;
        },
      },
      () => new Date('2026-08-22T12:00:00Z'),
    );
    await service.recordFailure(job('standard', 'review failed'));
    expect(await service.reconcileUpdater()).toMatchObject({ missing: 1, advanced: 0 });
    expect((await store.upgradesList())[0].state).toBe('proposed');
    expect((await store.capabilitiesList())[0].state).toBe('proposed');
  });

  it('ignores a stale updater observation after a later milestone', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'validation failed'));
    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'checking_shadow');
    await service.reconcileUpdater();
    expect((await store.upgradesList())[0].state).toBe('staging');

    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'proposal_verified', {
      deploymentId: null,
    });
    expect(await service.reconcileUpdater()).toMatchObject({ advanced: 0, failed: 0 });
    expect((await store.upgradesList())[0]).toMatchObject({
      state: 'staging',
      evidence: { updaterState: 'checking_shadow' },
    });
  });

  it('resumes from the last durable transition after an interrupted reconciliation', async () => {
    const store = new InterruptingStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'delivery failed'));
    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'completed');
    store.interruptAtStaging = true;

    expect(await service.reconcileUpdater()).toMatchObject({ failed: 1 });
    expect((await store.upgradesList())[0].state).toBe('reviewing');

    expect(await service.reconcileUpdater()).toMatchObject({ failed: 0 });
    expect((await store.upgradesList())[0].state).toBe('active');
    expect((await store.capabilitiesList())[0].state).toBe('active');
  });

  it('publishes an updater failure as rejected without claiming activation', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'quality gate failed'));
    updater.observed = await boundObservedUpgrade(store, proposal!.id, 'failed', {
      deploymentId: null,
      lastError: { code: 'required_check_failed', message: 'test failed' },
    });

    expect(await service.reconcileUpdater()).toMatchObject({ observed: 1, failed: 0 });
    expect((await store.upgradesList())[0]).toMatchObject({
      state: 'rejected',
      evidence: {
        updaterState: 'failed',
        failureCode: 'required_check_failed',
      },
    });
    expect((await store.capabilitiesList())[0].state).toBe('degraded');
    expect((await store.activity()).some((event) => event.kind === 'capability.activated')).toBe(
      false,
    );
  });

  it('rejects a signed proposal whose source handoff hash does not match', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'route failed'));
    updater.observed = observedUpgrade(proposal!.id, 'completed', {
      sourceHandoffSha256: '0'.repeat(64),
    });

    await expect(service.reconcileUpdater()).resolves.toMatchObject({
      observed: 1,
      advanced: 0,
      failed: 1,
    });
    expect((await store.upgradesList())[0]).toMatchObject({ state: 'proposed', evidence: {} });
    expect((await store.capabilitiesList())[0].state).toBe('proposed');
  });

  it('rejects an earlier handoff after the bound failure evidence changes', async () => {
    const store = new MemoryStore();
    const updater = new MutableUpdater();
    const service = new CapabilityService(store, updater, () => new Date('2026-08-22T12:00:00Z'));
    const proposal = await service.recordFailure(job('standard', 'route failed', 'a'));
    const signedAgainstEarlierEvidence = await boundObservedUpgrade(
      store,
      proposal!.id,
      'completed',
    );

    await service.recordFailure(job('standard', 'route failed', 'b'));
    updater.observed = signedAgainstEarlierEvidence;

    await expect(service.reconcileUpdater()).resolves.toMatchObject({
      observed: 1,
      advanced: 0,
      failed: 1,
    });
    expect((await store.upgradesList())[0]).toMatchObject({ state: 'proposed', evidence: {} });
  });
});

class MutableUpdater implements UpgradeStatusReader {
  observed?: ObservedUpgrade;

  async getByProposalId(proposalId: string): Promise<ObservedUpgrade | undefined> {
    return this.observed?.proposalId === proposalId ? structuredClone(this.observed) : undefined;
  }
}

class InterruptingStore extends MemoryStore {
  interruptAtStaging = false;

  override async saveUpgrade(upgrade: Parameters<MemoryStore['saveUpgrade']>[0]) {
    if (upgrade.state === 'staging' && this.interruptAtStaging) {
      this.interruptAtStaging = false;
      throw new Error('simulated process interruption');
    }
    return super.saveUpgrade(upgrade);
  }
}

function observedUpgrade(
  proposalId: string,
  state: ObservedUpgrade['state'],
  overrides: Partial<ObservedUpgrade> = {},
): ObservedUpgrade {
  return {
    id: '22222222-2222-4222-8222-222222222222',
    proposalId,
    sourceHandoffSha256: '9'.repeat(64),
    manifestSha256: 'a'.repeat(64),
    artifactSha256: 'b'.repeat(64),
    repository: {
      owner: 'mizuki-labs',
      name: 'mizuki',
      baseBranch: 'main',
      headBranch: `mizuki/${proposalId}`,
    },
    candidateSha: 'c'.repeat(40),
    attestations: {
      proposal: { keyId: 'proposal-key', sha256: 'a'.repeat(64) },
      benchmark: {
        receiptId: 'benchmark-1',
        keyId: 'benchmark-key',
        sha256: 'd'.repeat(64),
      },
      review: {
        receiptId: 'review-1',
        keyId: 'review-key',
        sha256: 'e'.repeat(64),
      },
    },
    state,
    prNumber: 42,
    prUrl: 'https://github.com/mizuki-labs/mizuki/pull/42',
    deploymentId: 'deployment-1',
    mergeSha: state === 'completed' ? 'f'.repeat(40) : null,
    promotionOperationId: state === 'completed' ? 'promotion-1' : null,
    promotionHealthyAt: state === 'completed' ? '2026-08-22T12:00:00.000Z' : null,
    nextAttemptAt: null,
    lastError: null,
    createdAt: '2026-08-22T11:00:00.000Z',
    updatedAt: '2026-08-22T12:00:00.000Z',
    auditHeadHash: '9'.repeat(64),
    ...overrides,
  };
}

async function boundObservedUpgrade(
  store: MemoryStore,
  proposalId: string,
  state: ObservedUpgrade['state'],
  overrides: Partial<ObservedUpgrade> = {},
): Promise<ObservedUpgrade> {
  const upgrade = (await store.upgradesList()).find((candidate) => candidate.id === proposalId);
  if (!upgrade) throw new Error('upgrade fixture was not found');
  const capability = (await store.capabilitiesList()).find(
    (candidate) => candidate.id === upgrade.capabilityId,
  );
  if (!capability) throw new Error('capability fixture was not found');
  const failures = await store.failuresForCapability(capability.key);
  const handoff = capabilityHandoff({ capability, upgrade, failures });
  return observedUpgrade(proposalId, state, {
    sourceHandoffSha256: handoff.handoffSha256,
    ...overrides,
  });
}

function job(jobClass: 'micro' | 'standard', error: string, suffix = 'a'): Job {
  return {
    id: `${suffix.repeat(8)}-${suffix.repeat(4)}-4${suffix.repeat(3)}-8${suffix.repeat(3)}-${suffix.repeat(12)}`,
    idempotencyKey: `key-${suffix}`,
    quote: {
      id: '11111111-1111-4111-8111-111111111111',
      issueUrl: 'https://github.com/example/project/issues/1',
      owner: 'example',
      repo: 'project',
      issueNumber: 1,
      issueTitle: 'Fix issue',
      issueBody: '',
      baseSha: '1'.repeat(40),
      defaultBranch: 'main',
      class: jobClass,
      priceAtomic: jobClass === 'micro' ? '2000000' : '10000000',
      maxFiles: jobClass === 'micro' ? 3 : 10,
      maxCostUsd: jobClass === 'micro' ? 0.8 : 4,
      validationCommands: [],
      expiresAt: '2099-01-01T00:00:00Z',
    },
    payment: { payer: '1'.repeat(32), transaction: 'tx', amountAtomic: '2000000' },
    state: 'refunded',
    error,
    createdAt: '2026-08-22T00:00:00Z',
    updatedAt: '2026-08-22T00:00:00Z',
    inputTokens: 0,
    outputTokens: 0,
    estimatedCostUsd: 0,
    version: 1,
  };
}
