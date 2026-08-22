import { calculateTreasuryWaterfall } from './domain/index.js';
import type { ServiceReadinessReport } from './readiness.js';
import type { MizukiStore } from './store.js';

const USDC_DECIMALS = 6;

export async function treasurySnapshot(store: MizukiStore, readiness?: ServiceReadinessReport) {
  const [jobs, ledger] = await Promise.all([store.jobsList(), store.ledgerEntries()]);
  const localOutstandingLiabilityAtomic = jobs
    .filter((job) => {
      if (job.state === 'refunded') return false;
      if (job.refundLiabilityId) return !job.refundLiabilityDischargedAt;
      return !['delivered', 'settlement_pending'].includes(job.state);
    })
    .reduce((total, job) => total + BigInt(job.payment.amountAtomic), 0n);
  const localOutstandingLiabilityUsd = atomicToUsd(localOutstandingLiabilityAtomic);
  const cutoff = Date.now() - 30 * 24 * 60 * 60_000;
  const trailingVariableAndOperatingEstimateUsd = sum(
    ledger
      .filter(
        (entry) =>
          ['route_cost', 'operating_cost'].includes(entry.kind) &&
          Date.parse(entry.createdAt) >= cutoff,
      )
      .map((entry) => entry.amountUsd),
  );
  const recordedInflowsUsd = sum(
    ledger
      .filter((entry) => ['customer_payment', 'treasury_deposit'].includes(entry.kind))
      .map((entry) => entry.amountUsd),
  );
  const recordedOutflowsUsd = sum(
    ledger
      .filter((entry) => ['route_cost', 'refund_completed', 'operating_cost'].includes(entry.kind))
      .map((entry) => entry.amountUsd),
  );
  const recordedNetFlowUsd = recordedInflowsUsd - recordedOutflowsUsd;
  const modeledFundsUsd = Math.max(0, recordedNetFlowUsd);
  const waterfall = calculateTreasuryWaterfall({
    liquidFundsCents: cents(modeledFundsUsd),
    settledUnfinishedLiabilitiesCents: cents(localOutstandingLiabilityUsd),
    trailingThirtyDayOperatingCostsCents: cents(trailingVariableAndOperatingEstimateUsd),
  });

  return {
    refundProtection: refundProtection(readiness, localOutstandingLiabilityAtomic),
    recordedInflowsUsd,
    recordedOutflowsUsd,
    recordedNetFlowUsd,
    localOutstandingLiabilityUsd,
    trailingVariableAndOperatingEstimateUsd,
    allocationModel: {
      source: 'application_ledger' as const,
      custodyVerified: false as const,
      modeledFundsUsd,
      targetsSatisfied: waterfall.reservesSatisfied,
      refundTargetUsd: waterfall.refundReserveTargetCents / 100,
      refundAllocationUsd: waterfall.refundReserveCents / 100,
      operatingTargetUsd: waterfall.operatingReserveTargetCents / 100,
      operatingAllocationUsd: waterfall.operatingReserveCents / 100,
      plannedImprovementAllocationUsd: waterfall.capabilityFundCents / 100,
      plannedResearchAllocationUsd: waterfall.researchFundCents / 100,
      policy: {
        refundTargetMinimumUsd: 50,
        operatingTargetMinimumUsd: 25,
        improvementShare: 0.7,
        researchShare: 0.3,
      },
    },
    updatedAt: new Date().toISOString(),
  };
}

function refundProtection(
  report: ServiceReadinessReport | undefined,
  localOutstandingLiabilityAtomic: bigint,
) {
  const unavailable = {
    status: 'unavailable' as const,
    source: null,
    refundTreasury: null,
    refundMint: null,
    refundDecimals: null,
    finalizedBalanceAtomic: null,
    signerOutstandingLiabilityAtomic: null,
    unencumberedBalanceAtomic: null,
    newIntakeCapacityAtomic: null,
    remainingDailyLimitUsdCents: null,
    localOutstandingLiabilityAtomic: localOutstandingLiabilityAtomic.toString(),
    liabilityReconciled: null,
    liabilitiesBacked: null,
    checkedAt: null,
  };
  if (!report?.ready) return unavailable;
  const dependency = report.dependencies.policy_signer;
  const evidence = dependency?.ok ? dependency.refundProtection : undefined;
  if (
    !evidence ||
    evidence.finalizedBalanceRaw === null ||
    evidence.pendingRefundRaw === null ||
    evidence.treasuryAvailableRefundRaw === null ||
    evidence.availableRefundRaw === null ||
    evidence.remainingRefundLimitUsdCents === null
  ) {
    return unavailable;
  }

  const finalizedBalance = BigInt(evidence.finalizedBalanceRaw);
  const signerOutstandingLiability = BigInt(evidence.pendingRefundRaw);
  const unencumberedBalance = BigInt(evidence.treasuryAvailableRefundRaw);
  const newIntakeCapacity = BigInt(evidence.availableRefundRaw);
  const expectedUnencumbered =
    finalizedBalance > signerOutstandingLiability
      ? finalizedBalance - signerOutstandingLiability
      : 0n;
  const dailyLimitAtomic =
    evidence.refundDecimals === USDC_DECIMALS
      ? BigInt(evidence.remainingRefundLimitUsdCents) * 10_000n
      : 0n;
  const expectedIntakeCapacity =
    unencumberedBalance < dailyLimitAtomic ? unencumberedBalance : dailyLimitAtomic;
  const liabilitiesBacked = finalizedBalance >= signerOutstandingLiability;
  const liabilityReconciled = signerOutstandingLiability === localOutstandingLiabilityAtomic;
  const coherent =
    evidence.refundDecimals === USDC_DECIMALS &&
    unencumberedBalance === expectedUnencumbered &&
    newIntakeCapacity === expectedIntakeCapacity;

  return {
    status:
      liabilitiesBacked && liabilityReconciled && coherent
        ? ('verified' as const)
        : ('degraded' as const),
    source: 'policy_signer_finalized' as const,
    refundTreasury: evidence.refundTreasury,
    refundMint: evidence.refundMint,
    refundDecimals: evidence.refundDecimals,
    finalizedBalanceAtomic: evidence.finalizedBalanceRaw,
    signerOutstandingLiabilityAtomic: evidence.pendingRefundRaw,
    unencumberedBalanceAtomic: evidence.treasuryAvailableRefundRaw,
    newIntakeCapacityAtomic: evidence.availableRefundRaw,
    remainingDailyLimitUsdCents: evidence.remainingRefundLimitUsdCents,
    localOutstandingLiabilityAtomic: localOutstandingLiabilityAtomic.toString(),
    liabilityReconciled,
    liabilitiesBacked,
    checkedAt: dependency.checkedAt,
  };
}

function atomicToUsd(value: bigint): number {
  const atomic = Number(value);
  if (!Number.isSafeInteger(atomic)) throw new Error('USDC amount exceeds safe accounting range');
  return atomic / 10 ** USDC_DECIMALS;
}

function cents(usd: number): number {
  return Math.max(0, Math.round(usd * 100));
}

function sum(values: number[]): number {
  return values.reduce((total, value) => total + value, 0);
}
