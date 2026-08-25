import { createHash } from 'node:crypto';
import {
  DomainRuleError,
  assertExpectedRevision,
  assertNonEmpty,
  assertNotBefore,
  assertTransition,
  timestampMs,
  type TransitionTable,
} from './state-machine.js';

export type JobClass = 'micro' | 'standard';

export type CapabilityState =
  | 'missing'
  | 'proposed'
  | 'funded'
  | 'implementing'
  | 'validating'
  | 'active'
  | 'degraded'
  | 'retired';

export type UpgradeState =
  | 'proposed'
  | 'approved'
  | 'funded'
  | 'implementing'
  | 'reviewing'
  | 'staging'
  | 'deployed'
  | 'active'
  | 'rolled_back'
  | 'rejected'
  | 'cancelled';

export type Capability = {
  id: string;
  key: string;
  name: string;
  state: CapabilityState;
  activeUpgradeId?: string;
  createdAt: string;
  updatedAt: string;
  revision: number;
};

export type UpgradeEvidence = {
  sourceHandoffHash?: string;
  updaterUpgradeId?: string;
  updaterState?: string;
  updaterAuditHash?: string;
  benchmarkReceiptId?: string;
  benchmarkReceiptHash?: string;
  benchmarkKeyId?: string;
  reviewReceiptId?: string;
  reviewReceiptHash?: string;
  reviewKeyId?: string;
  manifestHash?: string;
  proposalKeyId?: string;
  artifactHash?: string;
  candidateSha?: string;
  pullRequestUrl?: string;
  deploymentId?: string;
  mergeSha?: string;
  promotionOperationId?: string;
  promotionHealthyAt?: string;
  failureCode?: string;
};

export type Upgrade = {
  id: string;
  capabilityId: string;
  triggerReasons: readonly UpgradeTriggerReason[];
  state: UpgradeState;
  evidence: UpgradeEvidence;
  createdAt: string;
  updatedAt: string;
  revision: number;
};

export type FailureRecord = {
  id: string;
  capabilityKey: string;
  normalizedCode: string;
  jobClass: JobClass;
  occurredAt: string;
};

export type BenchmarkObservation = {
  baseline: number;
  current: number;
  direction: 'higher_is_better' | 'lower_is_better';
};

export type UpgradeTriggerReason =
  | 'paid_job_failure'
  | 'repeated_failure'
  | 'standard_job_failure'
  | 'benchmark_regression';

export type UpgradeTriggerDecision = {
  triggered: boolean;
  reasons: readonly UpgradeTriggerReason[];
  matchingFailures: number;
  benchmarkRegressionPercent?: number;
};

const SEVEN_DAYS_MS = 7 * 24 * 60 * 60 * 1_000;

const capabilityTransitions: TransitionTable<CapabilityState> = {
  missing: ['proposed', 'retired'],
  proposed: ['funded', 'implementing', 'degraded', 'missing', 'retired'],
  funded: ['implementing', 'degraded', 'retired'],
  implementing: ['validating', 'degraded', 'retired'],
  validating: ['active', 'degraded', 'implementing', 'retired'],
  active: ['degraded', 'retired'],
  degraded: ['proposed', 'validating', 'active', 'retired'],
  retired: [],
};

const upgradeTransitions: TransitionTable<UpgradeState> = {
  proposed: ['approved', 'rolled_back', 'rejected', 'cancelled'],
  approved: ['funded', 'implementing', 'rolled_back', 'rejected', 'cancelled'],
  funded: ['implementing', 'rolled_back', 'rejected', 'cancelled'],
  implementing: ['reviewing', 'rolled_back', 'rejected', 'cancelled'],
  reviewing: ['implementing', 'staging', 'rolled_back', 'rejected', 'cancelled'],
  staging: ['deployed', 'implementing', 'rolled_back', 'rejected', 'cancelled'],
  deployed: ['active', 'rolled_back'],
  active: ['rolled_back'],
  rolled_back: [],
  rejected: [],
  cancelled: [],
};

export function normalizeFailureCode(value: string): string {
  const source = String(value);
  const code = source
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '');
  const digest = createHash('sha256')
    .update(code || source)
    .digest('hex')
    .slice(0, 16);
  if (!code) return `failure_${digest}`;
  if (code.length <= 80) return code;
  return `${code.slice(0, 63).replace(/_+$/g, '')}_${digest}`;
}

export function evaluateUpgradeTrigger(input: {
  failure: FailureRecord;
  history: readonly FailureRecord[];
  benchmark?: BenchmarkObservation;
}): UpgradeTriggerDecision {
  const occurredAt = timestampMs(input.failure.occurredAt, 'failure time');
  const failureCode = normalizeFailureCode(input.failure.normalizedCode);
  const capabilityKey = assertNonEmpty(input.failure.capabilityKey, 'capability key');
  const seen = new Set([input.failure.id]);
  let matchingFailures = 1;

  for (const failure of input.history) {
    if (seen.has(failure.id)) continue;
    seen.add(failure.id);
    const historyAt = timestampMs(failure.occurredAt, 'failure history time');
    if (historyAt > occurredAt || occurredAt - historyAt > SEVEN_DAYS_MS) continue;
    if (failure.capabilityKey !== capabilityKey) continue;
    if (normalizeFailureCode(failure.normalizedCode) !== failureCode) continue;
    matchingFailures += 1;
  }

  const reasons: UpgradeTriggerReason[] = ['paid_job_failure'];
  if (matchingFailures >= 2) reasons.push('repeated_failure');
  if (input.failure.jobClass === 'standard') reasons.push('standard_job_failure');

  const benchmarkRegressionPercent = input.benchmark
    ? calculateBenchmarkRegressionPercent(input.benchmark)
    : undefined;
  if (benchmarkRegressionPercent !== undefined && benchmarkRegressionPercent > 10) {
    reasons.push('benchmark_regression');
  }

  return {
    triggered: reasons.length > 0,
    reasons,
    matchingFailures,
    ...(benchmarkRegressionPercent === undefined ? {} : { benchmarkRegressionPercent }),
  };
}

export function calculateBenchmarkRegressionPercent(observation: BenchmarkObservation): number {
  const { baseline, current, direction } = observation;
  if (!Number.isFinite(baseline) || !Number.isFinite(current) || baseline < 0 || current < 0) {
    throw new DomainRuleError(
      'INVALID_BENCHMARK',
      'Benchmark values must be finite, non-negative numbers',
    );
  }

  const degradation = direction === 'higher_is_better' ? baseline - current : current - baseline;
  if (degradation <= 0) return 0;
  if (baseline === 0) return Number.POSITIVE_INFINITY;
  return (degradation / baseline) * 100;
}

export function createCapability(input: {
  id: string;
  key: string;
  name: string;
  at: string;
}): Capability {
  const createdAt = new Date(timestampMs(input.at, 'created at')).toISOString();
  const key = assertNonEmpty(input.key, 'capability key').toLowerCase();
  if (!/^[a-z0-9]+(?:[._-][a-z0-9]+)*$/.test(key)) {
    throw new DomainRuleError('INVALID_CAPABILITY_KEY', 'Capability key is not valid');
  }
  return {
    id: assertNonEmpty(input.id, 'capability id'),
    key,
    name: assertNonEmpty(input.name, 'capability name'),
    state: 'missing',
    createdAt,
    updatedAt: createdAt,
    revision: 0,
  };
}

export function transitionCapability(
  capability: Capability,
  to: CapabilityState,
  input: {
    at: string;
    expectedRevision: number;
    activeUpgradeId?: string;
  },
): Capability {
  assertExpectedRevision(capability.revision, input.expectedRevision);
  assertNotBefore(input.at, capability.updatedAt, 'capability transition time');
  assertTransition(capabilityTransitions, capability.state, to, 'Capability');
  if (to === 'active' && !input.activeUpgradeId && !capability.activeUpgradeId) {
    throw new DomainRuleError('MISSING_ACTIVE_UPGRADE', 'Active capability requires an upgrade id');
  }
  return {
    ...capability,
    state: to,
    ...(input.activeUpgradeId
      ? { activeUpgradeId: assertNonEmpty(input.activeUpgradeId, 'active upgrade id') }
      : {}),
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: capability.revision + 1,
  };
}

export function createUpgrade(input: {
  id: string;
  capabilityId: string;
  triggerReasons: readonly UpgradeTriggerReason[];
  at: string;
}): Upgrade {
  if (input.triggerReasons.length === 0) {
    throw new DomainRuleError('MISSING_UPGRADE_TRIGGER', 'Upgrade requires at least one trigger');
  }
  const createdAt = new Date(timestampMs(input.at, 'created at')).toISOString();
  return {
    id: assertNonEmpty(input.id, 'upgrade id'),
    capabilityId: assertNonEmpty(input.capabilityId, 'capability id'),
    triggerReasons: [...new Set(input.triggerReasons)],
    state: 'proposed',
    evidence: {},
    createdAt,
    updatedAt: createdAt,
    revision: 0,
  };
}

export function transitionUpgrade(
  upgrade: Upgrade,
  to: UpgradeState,
  input: {
    at: string;
    expectedRevision: number;
    evidence?: UpgradeEvidence;
  },
): Upgrade {
  assertExpectedRevision(upgrade.revision, input.expectedRevision);
  assertNotBefore(input.at, upgrade.updatedAt, 'upgrade transition time');
  assertTransition(upgradeTransitions, upgrade.state, to, 'Upgrade');
  const evidence = compactEvidence({ ...upgrade.evidence, ...input.evidence });

  if (to === 'staging') {
    requireEvidence(evidence, ['benchmarkReceiptId', 'reviewReceiptId']);
  }
  if (to === 'deployed') {
    requireEvidence(evidence, [
      'benchmarkReceiptId',
      'reviewReceiptId',
      'manifestHash',
      'deploymentId',
    ]);
  }
  if (to === 'active') {
    requireEvidence(evidence, [
      'benchmarkReceiptId',
      'reviewReceiptId',
      'manifestHash',
      'deploymentId',
    ]);
  }

  return {
    ...upgrade,
    state: to,
    evidence,
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: upgrade.revision + 1,
  };
}

export function recordUpgradeEvidence(
  upgrade: Upgrade,
  input: {
    at: string;
    expectedRevision: number;
    evidence: UpgradeEvidence;
  },
): Upgrade {
  assertExpectedRevision(upgrade.revision, input.expectedRevision);
  assertNotBefore(input.at, upgrade.updatedAt, 'upgrade evidence time');
  return {
    ...upgrade,
    evidence: compactEvidence({ ...upgrade.evidence, ...input.evidence }),
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: upgrade.revision + 1,
  };
}

function compactEvidence(evidence: UpgradeEvidence): UpgradeEvidence {
  return Object.fromEntries(
    Object.entries(evidence)
      .filter(([, value]) => value !== undefined)
      .map(([key, value]) => [key, assertNonEmpty(value as string, key)]),
  );
}

function requireEvidence(
  evidence: UpgradeEvidence,
  fields: readonly (keyof UpgradeEvidence)[],
): void {
  const missing = fields.filter((field) => !evidence[field]);
  if (missing.length > 0) {
    throw new DomainRuleError(
      'MISSING_UPGRADE_EVIDENCE',
      `Upgrade is missing required evidence: ${missing.join(', ')}`,
    );
  }
}
