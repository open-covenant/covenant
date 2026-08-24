import { describe, expect, it } from 'vitest';
import { tractionTargets } from './flywheel';
import type { Metrics } from './types';

const metrics: Metrics = {
  paidJobs: 10,
  settlementPending: 0,
  settlementPendingOldestSeconds: null,
  deliveredPrs: 7,
  mergedPrs: 5,
  refundCount: 0,
  refundPending: 0,
  refundPendingOldestSeconds: null,
  refundSuccessRate: null,
  externalRepositories: 3,
  externalMaintainers: 3,
  settledCustomerReceiptsUsd: 34,
  recognizedRevenueUsd: 30,
  platformReportedCreatorFeesSentLamports: '2000000000',
  variableRouteCostEstimateUsd: 10,
  recognizedRevenueLessVariableRouteEstimateUsd: 20,
  grossMarginStatus: 'unverified',
  costCoverage: {
    included: [
      'gateway_model_token_rate_estimate',
      'gateway_sandbox_runtime_estimate',
      'reviewer_model_token_rate_estimate',
    ],
    excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
  },
  bountiesCreated: 2,
  bountiesOpen: 1,
  bountiesUnfundedOpen: 0,
  bountiesClaimed: 0,
  bountiesReleased: 1,
  externalContributors: 1,
  activeCapabilities: 1,
  refundProtection: {
    status: 'unavailable',
    source: null,
    refundTreasury: null,
    refundMint: null,
    refundDecimals: null,
    finalizedBalanceAtomic: null,
    signerOutstandingLiabilityAtomic: null,
    unencumberedBalanceAtomic: null,
    newIntakeCapacityAtomic: null,
    remainingDailyLimitUsdCents: null,
    localOutstandingLiabilityAtomic: '0',
    liabilityReconciled: null,
    liabilitiesBacked: null,
    checkedAt: null,
  },
  recordedNetFlowUsd: 31,
  plannedImprovementAllocationUsd: 4.2,
  plannedResearchAllocationUsd: 1.8,
  allocationTargetsSatisfied: true,
  updatedAt: '2026-08-22T12:00:00.000Z',
};

describe('tractionTargets', () => {
  it('does not claim refund success before a refund is finalized', () => {
    const refund = tractionTargets(metrics).find((target) => target.id === 'refunds');

    expect(refund).toMatchObject({ value: 'Not yet measured', met: false, progress: 0 });
  });

  it('requires every attempted refund to be finalized', () => {
    const refund = tractionTargets({
      ...metrics,
      refundCount: 1,
      refundPending: 1,
      refundSuccessRate: 0.5,
    }).find((target) => target.id === 'refunds');

    expect(refund).toMatchObject({ value: '50%', met: false, progress: 50 });
  });

  it('uses paid authorization receipts for the external-maintainer target', () => {
    const maintainer = tractionTargets({
      ...metrics,
      externalMaintainers: 2,
      externalRepositories: 5,
    }).find((target) => target.id === 'external-maintainers');

    expect(maintainer).toMatchObject({ value: '2', target: '3', met: false });
    expect(maintainer?.detail).toContain('5 external repositories');
  });

  it('keeps gross margin unverified while commercial costs are omitted', () => {
    const margin = tractionTargets(metrics).find((target) => target.id === 'margin');

    expect(margin).toMatchObject({ value: 'Not yet verified', met: false, progress: 0 });
    expect(margin?.detail).toContain('provider billing adjustments');
    expect(margin?.detail).toContain('after recorded AI model and sandbox costs');
    expect(margin?.detail).toContain('Solana network and payment-processing fees');
    expect(margin?.detail).toContain('infrastructure costs');
  });
});
