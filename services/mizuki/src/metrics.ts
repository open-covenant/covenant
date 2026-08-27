import type { Config } from './config.js';
import type { ServiceReadinessReport } from './readiness.js';
import type { MizukiStore } from './store.js';
import { treasurySnapshot } from './treasury.js';

export const publicCostCoverage = {
  included: [
    'gateway_model_token_rate_estimate',
    'gateway_sandbox_runtime_estimate',
    'reviewer_model_token_rate_estimate',
  ],
  excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
} as const;

export async function metrics(
  config: Config,
  store: MizukiStore,
  readiness?: ServiceReadinessReport,
) {
  const [jobs, bounties, capabilities, treasury, ledger] = await Promise.all([
    store.jobsList(),
    store.bountiesList(),
    store.capabilitiesList(),
    treasurySnapshot(store, readiness),
    store.ledgerEntries(),
  ]);
  const paid = jobs.filter(
    (job) => job.state !== 'settlement_pending' && job.state !== 'payment_expired',
  );
  const settlementPending = jobs.filter((job) => job.state === 'settlement_pending');
  const delivered = jobs.filter((job) => job.state === 'delivered');
  const merged = delivered.filter((job) => job.mergedAt);
  const recognized = delivered.filter((job) => job.refundLiabilityDischargedAt);
  const refundObligations = jobs.filter((job) =>
    ['rejected', 'failed', 'refund_pending', 'refunded'].includes(job.state),
  );
  const refunds = refundObligations.filter((job) => job.state === 'refunded');
  const refundPending = refundObligations.filter((job) => job.state !== 'refunded');
  const externalJobs = jobs.filter((job) => {
    if (job.state === 'settlement_pending' || job.state === 'payment_expired') return false;
    if (!job.quote.installationId || !job.quote.authorizationReceipt) return false;
    return !config.internalRepos.has(`${job.quote.owner}/${job.quote.repo}`.toLowerCase());
  });
  const externalRepos = new Set(
    externalJobs.map((job) => `${job.quote.owner}/${job.quote.repo}`.toLowerCase()),
  );
  const externalMaintainers = new Set(
    externalJobs.flatMap((job) =>
      job.quote.authorizationReceipt ? [job.quote.authorizationReceipt.actorId] : [],
    ),
  );
  const openBounties = bounties.filter((bounty) => bounty.state === 'open');
  const openEscrows = await Promise.all(
    openBounties.map((bounty) => store.escrowByBounty(bounty.id)),
  );
  const bountiesUnfundedOpen = openEscrows.filter(
    (escrow) =>
      !escrow?.fundingSignature ||
      ['requested', 'funding', 'failed', 'refunded'].includes(escrow.state),
  ).length;
  const settledCustomerReceiptsUsd = sum(
    paid.map((job) => Number(job.payment.amountAtomic) / 1_000_000),
  );
  const recognizedRevenueUsd = sum(
    recognized.map((job) => Number(job.payment.amountAtomic) / 1_000_000),
  );
  const variableRouteCostEstimateUsd = sum(jobs.map((job) => job.estimatedCostUsd));
  const recognizedRevenueLessVariableRouteEstimateUsd =
    recognizedRevenueUsd - variableRouteCostEstimateUsd;
  const creatorFeePrefix = config.clawPumpAgentId
    ? `clawpump:${config.clawPumpAgentId}:`
    : undefined;
  const platformReportedCreatorFeesSentLamports = creatorFeePrefix
    ? ledger
        .filter(
          (entry) =>
            entry.kind === 'creator_fee' &&
            entry.asset === 'SOL' &&
            entry.referenceId.startsWith(creatorFeePrefix),
        )
        .reduce((total, entry) => total + BigInt(entry.amountAtomic), 0n)
        .toString()
    : '0';
  return {
    paidJobs: paid.length,
    settlementPending: settlementPending.length,
    settlementPendingOldestSeconds: oldestAgeSeconds(settlementPending),
    deliveredPrs: delivered.length,
    mergedPrs: merged.length,
    refundCount: refunds.length,
    refundPending: refundPending.length,
    refundPendingOldestSeconds: oldestAgeSeconds(refundPending),
    refundSuccessRate:
      refundObligations.length === 0 ? null : refunds.length / refundObligations.length,
    externalRepositories: externalRepos.size,
    externalMaintainers: externalMaintainers.size,
    settledCustomerReceiptsUsd,
    recognizedRevenueUsd,
    platformReportedCreatorFeesSentLamports,
    variableRouteCostEstimateUsd,
    recognizedRevenueLessVariableRouteEstimateUsd,
    grossMarginStatus: 'unverified' as const,
    costCoverage: publicCostCoverage,
    bountiesCreated: bounties.length,
    bountiesOpen: openBounties.length,
    bountiesUnfundedOpen,
    bountiesClaimed: bounties.filter((bounty) =>
      ['claimed', 'pr_submitted', 'validating', 'accepted'].includes(bounty.state),
    ).length,
    bountiesReleased: bounties.filter((bounty) => bounty.state === 'released').length,
    externalContributors: new Set(
      bounties.flatMap((bounty) => (bounty.activeClaim ? [bounty.activeClaim.claimantId] : [])),
    ).size,
    activeCapabilities: capabilities.filter((capability) => capability.state === 'active').length,
    refundProtection: treasury.refundProtection,
    recordedNetFlowUsd: treasury.recordedNetFlowUsd,
    plannedImprovementAllocationUsd: treasury.allocationModel.plannedImprovementAllocationUsd,
    plannedResearchAllocationUsd: treasury.allocationModel.plannedResearchAllocationUsd,
    allocationTargetsSatisfied: treasury.allocationModel.targetsSatisfied,
    tokenMint: config.tokenMint ?? null,
    updatedAt: new Date().toISOString(),
  };
}

export function prometheus(value: Awaited<ReturnType<typeof metrics>>): string {
  return [
    '# TYPE mizuki_paid_jobs_total counter',
    `mizuki_paid_jobs_total ${value.paidJobs}`,
    '# TYPE mizuki_settlement_pending gauge',
    `mizuki_settlement_pending ${value.settlementPending}`,
    '# TYPE mizuki_settlement_pending_oldest_seconds gauge',
    `mizuki_settlement_pending_oldest_seconds ${value.settlementPendingOldestSeconds ?? 'NaN'}`,
    '# TYPE mizuki_delivered_prs_total counter',
    `mizuki_delivered_prs_total ${value.deliveredPrs}`,
    '# TYPE mizuki_merged_prs_total counter',
    `mizuki_merged_prs_total ${value.mergedPrs}`,
    '# TYPE mizuki_refunds_total counter',
    `mizuki_refunds_total ${value.refundCount}`,
    '# TYPE mizuki_refund_pending gauge',
    `mizuki_refund_pending ${value.refundPending}`,
    '# TYPE mizuki_refund_pending_oldest_seconds gauge',
    `mizuki_refund_pending_oldest_seconds ${value.refundPendingOldestSeconds ?? 'NaN'}`,
    '# TYPE mizuki_refund_success_ratio gauge',
    `mizuki_refund_success_ratio ${value.refundSuccessRate ?? 'NaN'}`,
    '# TYPE mizuki_external_repositories gauge',
    `mizuki_external_repositories ${value.externalRepositories}`,
    '# TYPE mizuki_external_maintainers gauge',
    `mizuki_external_maintainers ${value.externalMaintainers}`,
    '# TYPE mizuki_settled_customer_receipts_usd gauge',
    `mizuki_settled_customer_receipts_usd ${value.settledCustomerReceiptsUsd}`,
    '# TYPE mizuki_recognized_revenue_usd gauge',
    `mizuki_recognized_revenue_usd ${value.recognizedRevenueUsd}`,
    '# TYPE mizuki_platform_reported_creator_fees_sent_lamports gauge',
    `mizuki_platform_reported_creator_fees_sent_lamports ${value.platformReportedCreatorFeesSentLamports}`,
    '# TYPE mizuki_variable_route_cost_estimate_usd gauge',
    `mizuki_variable_route_cost_estimate_usd ${value.variableRouteCostEstimateUsd}`,
    '# TYPE mizuki_recognized_revenue_less_variable_route_estimate_usd gauge',
    `mizuki_recognized_revenue_less_variable_route_estimate_usd ${value.recognizedRevenueLessVariableRouteEstimateUsd}`,
    '# TYPE mizuki_gross_margin_verified gauge',
    'mizuki_gross_margin_verified 0',
    '# TYPE mizuki_bounties_created_total counter',
    `mizuki_bounties_created_total ${value.bountiesCreated}`,
    '# TYPE mizuki_bounties_open gauge',
    `mizuki_bounties_open ${value.bountiesOpen}`,
    '# TYPE mizuki_bounties_unfunded_open gauge',
    `mizuki_bounties_unfunded_open ${value.bountiesUnfundedOpen}`,
    '# TYPE mizuki_bounties_released_total counter',
    `mizuki_bounties_released_total ${value.bountiesReleased}`,
    '# TYPE mizuki_external_contributors gauge',
    `mizuki_external_contributors ${value.externalContributors}`,
    '# TYPE mizuki_refund_protection_verified gauge',
    `mizuki_refund_protection_verified ${value.refundProtection.status === 'verified' ? 1 : 0}`,
    '# TYPE mizuki_refund_liability_reconciled gauge',
    `mizuki_refund_liability_reconciled ${booleanMetric(value.refundProtection.liabilityReconciled)}`,
    '# TYPE mizuki_signer_finalized_refund_balance_atomic gauge',
    `mizuki_signer_finalized_refund_balance_atomic ${atomicMetric(value.refundProtection.finalizedBalanceAtomic)}`,
    '# TYPE mizuki_signer_outstanding_refund_liability_atomic gauge',
    `mizuki_signer_outstanding_refund_liability_atomic ${atomicMetric(value.refundProtection.signerOutstandingLiabilityAtomic)}`,
    '# TYPE mizuki_signer_new_intake_capacity_atomic gauge',
    `mizuki_signer_new_intake_capacity_atomic ${atomicMetric(value.refundProtection.newIntakeCapacityAtomic)}`,
    '# TYPE mizuki_recorded_net_flow_usd gauge',
    `mizuki_recorded_net_flow_usd ${value.recordedNetFlowUsd}`,
    '# TYPE mizuki_planned_improvement_allocation_usd gauge',
    `mizuki_planned_improvement_allocation_usd ${value.plannedImprovementAllocationUsd}`,
    '',
  ].join('\n');
}

function atomicMetric(value: string | null): string {
  return value ?? 'NaN';
}

function booleanMetric(value: boolean | null): string | number {
  return value === null ? 'NaN' : value ? 1 : 0;
}

function sum(values: number[]): number {
  return values.reduce((total, value) => total + value, 0);
}

function oldestAgeSeconds(values: Array<{ updatedAt: string }>): number | null {
  if (values.length === 0) return null;
  const oldest = values.reduce(
    (minimum, value) => Math.min(minimum, Date.parse(value.updatedAt)),
    Number.POSITIVE_INFINITY,
  );
  return Math.max(0, Math.floor((Date.now() - oldest) / 1_000));
}
