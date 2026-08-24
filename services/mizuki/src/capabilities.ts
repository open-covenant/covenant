import { randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { capabilityHandoff } from './capability-handoff.js';
import {
  createCapability,
  createUpgrade,
  evaluateUpgradeTrigger,
  normalizeFailureCode,
  recordUpgradeEvidence,
  transitionCapability,
  transitionUpgrade,
  type Capability,
  type CapabilityState,
  type FailureRecord,
  type Upgrade,
  type UpgradeEvidence,
  type UpgradeState,
} from './domain/index.js';
import type { MizukiStore } from './store.js';
import type { Job } from './types.js';
import type {
  ObservedUpdaterState,
  ObservedUpgrade,
  UpgradeStatusReader,
} from './updater-client.js';

export interface CapabilityReconcileResult {
  observed: number;
  advanced: number;
  missing: number;
  failed: number;
}

const updaterStateRank: Record<ObservedUpdaterState, number> = {
  submitted: 0,
  verifying_artifact: 1,
  proposal_verified: 2,
  syncing_pr: 3,
  waiting_checks: 4,
  starting_shadow: 5,
  checking_shadow: 6,
  merging: 7,
  promoting: 8,
  verifying_promotion: 9,
  completed: 10,
  rollback_pending: 10,
  rolled_back: 11,
  failed: 11,
  rollback_failed: 11,
};

const activeUpgradeStates = new Set<UpgradeState>([
  'proposed',
  'approved',
  'funded',
  'implementing',
  'reviewing',
  'staging',
  'deployed',
]);

export class CapabilityService {
  constructor(
    private readonly store: MizukiStore,
    private readonly updater?: UpgradeStatusReader,
    private readonly now: () => Date = () => new Date(),
  ) {}

  async recordFailure(job: Job): Promise<Upgrade | undefined> {
    const capabilityKey = classifyCapability(job.error ?? 'maintenance failure');
    const failure: FailureRecord = {
      id: job.id,
      capabilityKey,
      normalizedCode: normalizeFailureCode(job.error ?? 'maintenance_failure'),
      jobClass: job.quote.class,
      occurredAt: this.now().toISOString(),
    };
    const history = await this.store.failuresForCapability(capabilityKey);
    await this.store.saveFailure(failure);
    const decision = evaluateUpgradeTrigger({ failure, history });
    if (!decision.triggered) return undefined;

    let capability = await this.store.capabilityByKey(capabilityKey);
    if (!capability) {
      capability = createCapability({
        id: randomUUID(),
        key: capabilityKey,
        name: capabilityName(capabilityKey),
        at: this.now().toISOString(),
      });
      try {
        await this.store.saveCapability(capability);
      } catch {
        capability = await this.store.capabilityByKey(capabilityKey);
        if (!capability) throw new Error('capability creation conflicted');
      }
    }

    const upgrades = await this.store.upgradesList();
    const activeProposal = upgrades.find(
      (upgrade) =>
        upgrade.capabilityId === capability!.id && activeUpgradeStates.has(upgrade.state),
    );
    if (activeProposal) return activeProposal;

    capability = await this.propose(capability);
    const upgrade = createUpgrade({
      id: randomUUID(),
      capabilityId: capability.id,
      triggerReasons: decision.reasons,
      at: this.now().toISOString(),
    });
    await this.store.saveUpgrade(upgrade);
    await this.store.appendActivity('capability.proposed', capability.id, {
      capabilityKey,
      upgradeId: upgrade.id,
      reasons: decision.reasons,
      matchingFailures: decision.matchingFailures,
    });
    return upgrade;
  }

  async reconcileUpdater(): Promise<CapabilityReconcileResult> {
    const result: CapabilityReconcileResult = {
      observed: 0,
      advanced: 0,
      missing: 0,
      failed: 0,
    };
    if (!this.updater) return result;

    const upgrades = await this.store.upgradesList();
    const capabilities = new Map(
      (await this.store.capabilitiesList()).map((capability) => [capability.id, capability]),
    );
    for (const upgrade of upgrades.filter((candidate) =>
      activeUpgradeStates.has(candidate.state),
    )) {
      try {
        const observed = await this.updater.getByProposalId(upgrade.id);
        if (!observed) {
          result.missing += 1;
          continue;
        }
        result.observed += 1;
        const capability = capabilities.get(upgrade.capabilityId);
        if (!capability) throw new Error(`capability ${upgrade.capabilityId} was not found`);
        if (observed.proposalId !== upgrade.id) {
          throw new Error('updater observation is not bound to the requested proposal');
        }
        const failures = await this.store.failuresForCapability(capability.key);
        const expectedHandoff = capabilityHandoff({ capability, upgrade, failures });
        if (observed.sourceHandoffSha256 !== expectedHandoff.handoffSha256) {
          throw new Error('signed updater proposal is not bound to the current capability handoff');
        }
        const reconciled = await this.reconcileUpgrade(upgrade, capability, observed);
        result.advanced += reconciled.transitions;
        capabilities.set(capability.id, reconciled.capability);
      } catch {
        result.failed += 1;
      }
    }
    return result;
  }

  private async reconcileUpgrade(
    original: Upgrade,
    originalCapability: Capability,
    observed: ObservedUpgrade,
  ): Promise<{ upgrade: Upgrade; capability: Capability; transitions: number }> {
    const priorUpdaterState = original.evidence.updaterState as ObservedUpdaterState | undefined;
    if (
      priorUpdaterState &&
      updaterStateRank[observed.state] < updaterStateRank[priorUpdaterState]
    ) {
      return { upgrade: original, capability: originalCapability, transitions: 0 };
    }

    const evidence = updaterEvidence(observed);
    let upgrade = original;
    let transitions = 0;
    const target = targetUpgradeState(observed.state);

    if (target === 'rejected' || target === 'rolled_back') {
      if (upgrade.state !== target) {
        upgrade = transitionUpgrade(upgrade, target, {
          at: this.now().toISOString(),
          expectedRevision: upgrade.revision,
          evidence,
        });
        await this.store.saveUpgrade(upgrade);
        transitions += 1;
      }
    } else {
      while (upgrade.state !== target) {
        const next = nextHappyState(upgrade.state, target);
        if (!next) break;
        upgrade = transitionUpgrade(upgrade, next, {
          at: this.now().toISOString(),
          expectedRevision: upgrade.revision,
          evidence,
        });
        await this.store.saveUpgrade(upgrade);
        transitions += 1;
      }
    }

    if (!isDeepStrictEqual(upgrade.evidence, evidence)) {
      upgrade = recordUpgradeEvidence(upgrade, {
        at: this.now().toISOString(),
        expectedRevision: upgrade.revision,
        evidence,
      });
      await this.store.saveUpgrade(upgrade);
      transitions += 1;
    }

    const capability = await this.reconcileCapability(originalCapability, upgrade);
    if (capability.state === 'active' && originalCapability.state !== 'active') {
      await this.store.appendActivity('capability.activated', capability.id, {
        upgradeId: upgrade.id,
        updaterUpgradeId: observed.id,
        auditHash: observed.auditHeadHash,
      });
    }
    if (observed.state === 'rolled_back' && originalCapability.state !== 'degraded') {
      await this.store.appendActivity('capability.rolled_back', capability.id, {
        upgradeId: upgrade.id,
        updaterUpgradeId: observed.id,
        auditHash: observed.auditHeadHash,
      });
    }
    return { upgrade, capability, transitions };
  }

  private async reconcileCapability(original: Capability, upgrade: Upgrade): Promise<Capability> {
    let capability = original;
    const target = targetCapabilityState(upgrade.state);
    while (capability.state !== target) {
      const next = nextCapabilityState(capability.state, target);
      if (!next) break;
      capability = transitionCapability(capability, next, {
        at: this.now().toISOString(),
        expectedRevision: capability.revision,
        ...(next === 'active' ? { activeUpgradeId: upgrade.id } : {}),
      });
      await this.store.saveCapability(capability);
    }
    return capability;
  }

  private async propose(capability: Capability): Promise<Capability> {
    if (capability.state === 'proposed') return capability;
    if (capability.state === 'missing' || capability.state === 'degraded') {
      const proposed = transitionCapability(capability, 'proposed', {
        at: this.now().toISOString(),
        expectedRevision: capability.revision,
      });
      await this.store.saveCapability(proposed);
      return proposed;
    }
    if (capability.state === 'active') {
      const degraded = transitionCapability(capability, 'degraded', {
        at: this.now().toISOString(),
        expectedRevision: capability.revision,
      });
      await this.store.saveCapability(degraded);
      const proposed = transitionCapability(degraded, 'proposed', {
        at: this.now().toISOString(),
        expectedRevision: degraded.revision,
      });
      await this.store.saveCapability(proposed);
      return proposed;
    }
    return capability;
  }
}

function targetUpgradeState(state: ObservedUpdaterState): UpgradeState {
  switch (state) {
    case 'submitted':
    case 'verifying_artifact':
      return 'proposed';
    case 'proposal_verified':
    case 'syncing_pr':
    case 'waiting_checks':
      return 'reviewing';
    case 'starting_shadow':
    case 'checking_shadow':
    case 'merging':
    case 'promoting':
    case 'verifying_promotion':
    case 'rollback_pending':
      return 'staging';
    case 'completed':
      return 'active';
    case 'rolled_back':
      return 'rolled_back';
    case 'failed':
    case 'rollback_failed':
      return 'rejected';
  }
}

function nextHappyState(current: UpgradeState, target: UpgradeState): UpgradeState | undefined {
  const rank: Partial<Record<UpgradeState, number>> = {
    proposed: 0,
    approved: 1,
    funded: 1,
    implementing: 2,
    reviewing: 3,
    staging: 4,
    deployed: 5,
    active: 6,
  };
  const currentRank = rank[current];
  const targetRank = rank[target];
  if (currentRank === undefined || targetRank === undefined || currentRank >= targetRank) {
    return undefined;
  }
  if (current === 'proposed') return 'approved';
  if (current === 'approved' || current === 'funded') return 'implementing';
  if (current === 'implementing') return 'reviewing';
  if (current === 'reviewing') return 'staging';
  if (current === 'staging') return 'deployed';
  if (current === 'deployed') return 'active';
  return undefined;
}

function targetCapabilityState(state: UpgradeState): CapabilityState {
  switch (state) {
    case 'proposed':
    case 'approved':
    case 'funded':
      return 'proposed';
    case 'implementing':
    case 'reviewing':
      return 'implementing';
    case 'staging':
    case 'deployed':
      return 'validating';
    case 'active':
      return 'active';
    case 'rolled_back':
    case 'rejected':
    case 'cancelled':
      return 'degraded';
  }
}

function nextCapabilityState(
  current: CapabilityState,
  target: CapabilityState,
): CapabilityState | undefined {
  if (target === 'degraded') {
    if (current === 'missing') return 'proposed';
    return current === 'degraded' || current === 'retired' ? undefined : 'degraded';
  }
  if (target === 'proposed') {
    if (current === 'missing' || current === 'degraded') return 'proposed';
    return undefined;
  }
  if (target === 'implementing') {
    if (current === 'missing' || current === 'degraded') return 'proposed';
    if (current === 'proposed' || current === 'funded') return 'implementing';
    return undefined;
  }
  if (target === 'validating') {
    if (current === 'missing' || current === 'proposed' || current === 'funded') {
      return current === 'missing' ? 'proposed' : 'implementing';
    }
    if (current === 'degraded') return 'validating';
    if (current === 'implementing') return 'validating';
    return undefined;
  }
  if (target === 'active') {
    if (current === 'missing' || current === 'proposed' || current === 'funded') {
      return current === 'missing' ? 'proposed' : 'implementing';
    }
    if (current === 'degraded' || current === 'implementing') return 'validating';
    if (current === 'validating') return 'active';
  }
  return undefined;
}

function updaterEvidence(observed: ObservedUpgrade): UpgradeEvidence {
  return {
    sourceHandoffHash: observed.sourceHandoffSha256,
    updaterUpgradeId: observed.id,
    updaterState: observed.state,
    updaterAuditHash: observed.auditHeadHash,
    benchmarkReceiptId: observed.attestations.benchmark.receiptId,
    benchmarkReceiptHash: observed.attestations.benchmark.sha256,
    benchmarkKeyId: observed.attestations.benchmark.keyId,
    reviewReceiptId: observed.attestations.review.receiptId,
    reviewReceiptHash: observed.attestations.review.sha256,
    reviewKeyId: observed.attestations.review.keyId,
    manifestHash: observed.manifestSha256,
    proposalKeyId: observed.attestations.proposal.keyId,
    artifactHash: observed.artifactSha256,
    candidateSha: observed.candidateSha,
    ...(observed.prUrl ? { pullRequestUrl: observed.prUrl } : {}),
    ...(observed.deploymentId ? { deploymentId: observed.deploymentId } : {}),
    ...(observed.mergeSha ? { mergeSha: observed.mergeSha } : {}),
    ...(observed.promotionOperationId
      ? { promotionOperationId: observed.promotionOperationId }
      : {}),
    ...(observed.promotionHealthyAt ? { promotionHealthyAt: observed.promotionHealthyAt } : {}),
    ...(observed.lastError ? { failureCode: observed.lastError.code } : {}),
  };
}

function classifyCapability(error: string): string {
  if (/route|model|inference|usepod/i.test(error)) return 'model.route-reliability';
  if (/review|quality|repair/i.test(error)) return 'patch.quality';
  if (/validat|test|check/i.test(error)) return 'repository.validation';
  if (/scope|forbidden|too large|policy/i.test(error)) return 'scope.classification';
  if (/github|pull request|repository head/i.test(error)) return 'github.delivery';
  if (/timeout|timed out/i.test(error)) return 'execution.timeout';
  return 'maintenance.general';
}

function capabilityName(key: string): string {
  return key
    .split(/[.-]/)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ');
}
