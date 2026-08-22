import { describe, expect, it } from 'vitest';
import {
  calculateBenchmarkRegressionPercent,
  createCapability,
  createUpgrade,
  evaluateUpgradeTrigger,
  normalizeFailureCode,
  transitionCapability,
  transitionUpgrade,
  type FailureRecord,
} from './capability.js';
import { DomainRuleError } from './state-machine.js';

const T0 = '2026-08-22T10:00:00.000Z';

function failure(overrides: Partial<FailureRecord> = {}): FailureRecord {
  return {
    id: 'failure-current',
    capabilityKey: 'typescript-repair',
    normalizedCode: 'validation_failed',
    jobClass: 'micro',
    occurredAt: T0,
    ...overrides,
  };
}

describe('failure-to-upgrade triggers', () => {
  it('normalizes failure codes for stable grouping', () => {
    expect(normalizeFailureCode(' Validation Failed: TypeScript ')).toBe(
      'validation_failed_typescript',
    );
    expect(() => normalizeFailureCode('---')).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'INVALID_FAILURE_CODE' }),
    );
  });

  it('publishes the first paid failure and records a repeated failure within seven days', () => {
    const decision = evaluateUpgradeTrigger({
      failure: failure(),
      history: [
        failure({
          id: 'failure-prior',
          normalizedCode: 'Validation Failed',
          occurredAt: '2026-08-15T10:00:00.000Z',
        }),
      ],
    });
    expect(decision).toEqual({
      triggered: true,
      reasons: ['paid_job_failure', 'repeated_failure'],
      matchingFailures: 2,
    });
  });

  it('ignores unrelated history without suppressing the paid-failure trigger', () => {
    const current = failure();
    const decision = evaluateUpgradeTrigger({
      failure: current,
      history: [
        current,
        failure({ id: 'old', occurredAt: '2026-08-15T09:59:59.999Z' }),
        failure({ id: 'future', occurredAt: '2026-08-22T10:00:00.001Z' }),
        failure({ id: 'other', capabilityKey: 'python-repair' }),
      ],
    });
    expect(decision).toEqual({
      triggered: true,
      reasons: ['paid_job_failure'],
      matchingFailures: 1,
    });
  });

  it('always triggers for a failed Standard job', () => {
    expect(
      evaluateUpgradeTrigger({
        failure: failure({ jobClass: 'standard' }),
        history: [],
      }),
    ).toMatchObject({
      triggered: true,
      reasons: ['paid_job_failure', 'standard_job_failure'],
    });
  });

  it('adds a benchmark trigger only when regression exceeds ten percent', () => {
    expect(
      evaluateUpgradeTrigger({
        failure: failure(),
        history: [],
        benchmark: { baseline: 100, current: 90, direction: 'higher_is_better' },
      }),
    ).toMatchObject({
      triggered: true,
      reasons: ['paid_job_failure'],
      benchmarkRegressionPercent: 10,
    });
    expect(
      evaluateUpgradeTrigger({
        failure: failure(),
        history: [],
        benchmark: { baseline: 100, current: 89, direction: 'higher_is_better' },
      }),
    ).toMatchObject({
      triggered: true,
      reasons: ['paid_job_failure', 'benchmark_regression'],
      benchmarkRegressionPercent: 11,
    });
    expect(
      evaluateUpgradeTrigger({
        failure: failure(),
        history: [],
        benchmark: { baseline: 100, current: 111, direction: 'lower_is_better' },
      }),
    ).toMatchObject({
      triggered: true,
      reasons: ['paid_job_failure', 'benchmark_regression'],
      benchmarkRegressionPercent: 11,
    });
  });

  it('handles a zero baseline without hiding a regression', () => {
    expect(
      calculateBenchmarkRegressionPercent({
        baseline: 0,
        current: 1,
        direction: 'lower_is_better',
      }),
    ).toBe(Number.POSITIVE_INFINITY);
    expect(
      calculateBenchmarkRegressionPercent({
        baseline: 0,
        current: 1,
        direction: 'higher_is_better',
      }),
    ).toBe(0);
  });
});

describe('capability lifecycle', () => {
  it('requires a verified upgrade when activating a capability', () => {
    let capability = createCapability({
      id: 'capability-1',
      key: 'typescript.repair',
      name: 'TypeScript repair',
      at: T0,
    });
    capability = transitionCapability(capability, 'proposed', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    capability = transitionCapability(capability, 'funded', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: 1,
    });
    capability = transitionCapability(capability, 'implementing', {
      at: '2026-08-22T10:03:00.000Z',
      expectedRevision: 2,
    });
    capability = transitionCapability(capability, 'validating', {
      at: '2026-08-22T10:04:00.000Z',
      expectedRevision: 3,
    });
    expect(() =>
      transitionCapability(capability, 'active', {
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 4,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'MISSING_ACTIVE_UPGRADE',
      }),
    );
    capability = transitionCapability(capability, 'active', {
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 4,
      activeUpgradeId: 'upgrade-1',
    });
    expect(capability).toMatchObject({
      state: 'active',
      activeUpgradeId: 'upgrade-1',
      revision: 5,
    });
  });

  it('does not allow retired capabilities to re-enter service', () => {
    const capability = transitionCapability(
      createCapability({
        id: 'capability-1',
        key: 'typescript',
        name: 'TypeScript',
        at: T0,
      }),
      'retired',
      {
        at: '2026-08-22T10:01:00.000Z',
        expectedRevision: 0,
      },
    );
    expect(() =>
      transitionCapability(capability, 'proposed', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrow();
  });
});

describe('upgrade lifecycle', () => {
  function approvedUpgrade() {
    const proposal = createUpgrade({
      id: 'upgrade-1',
      capabilityId: 'capability-1',
      triggerReasons: ['repeated_failure', 'repeated_failure'],
      at: T0,
    });
    return transitionUpgrade(proposal, 'approved', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
  }

  it('deduplicates triggers and enforces review evidence before staging', () => {
    let upgrade = approvedUpgrade();
    expect(upgrade.triggerReasons).toEqual(['repeated_failure']);
    upgrade = transitionUpgrade(upgrade, 'funded', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: 1,
    });
    upgrade = transitionUpgrade(upgrade, 'implementing', {
      at: '2026-08-22T10:03:00.000Z',
      expectedRevision: 2,
    });
    upgrade = transitionUpgrade(upgrade, 'reviewing', {
      at: '2026-08-22T10:04:00.000Z',
      expectedRevision: 3,
    });
    expect(() =>
      transitionUpgrade(upgrade, 'staging', {
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 4,
        evidence: { benchmarkReceiptId: 'benchmark-1' },
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'MISSING_UPGRADE_EVIDENCE',
      }),
    );
  });

  it('activates only with complete benchmark, review, manifest, and deployment evidence', () => {
    let upgrade = approvedUpgrade();
    const transitions = ['funded', 'implementing', 'reviewing'] as const;
    for (const [index, state] of transitions.entries()) {
      upgrade = transitionUpgrade(upgrade, state, {
        at: `2026-08-22T10:0${index + 2}:00.000Z`,
        expectedRevision: index + 1,
      });
    }
    upgrade = transitionUpgrade(upgrade, 'staging', {
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 4,
      evidence: { benchmarkReceiptId: 'benchmark-1', reviewReceiptId: 'review-1' },
    });
    upgrade = transitionUpgrade(upgrade, 'deployed', {
      at: '2026-08-22T10:06:00.000Z',
      expectedRevision: 5,
      evidence: { manifestHash: 'manifest-1', deploymentId: 'deployment-1' },
    });
    upgrade = transitionUpgrade(upgrade, 'active', {
      at: '2026-08-22T10:07:00.000Z',
      expectedRevision: 6,
    });
    expect(upgrade).toMatchObject({
      state: 'active',
      evidence: {
        benchmarkReceiptId: 'benchmark-1',
        reviewReceiptId: 'review-1',
        manifestHash: 'manifest-1',
        deploymentId: 'deployment-1',
      },
      revision: 7,
    });
    const rolledBack = transitionUpgrade(upgrade, 'rolled_back', {
      at: '2026-08-22T10:08:00.000Z',
      expectedRevision: 7,
    });
    expect(rolledBack.state).toBe('rolled_back');
  });
});
