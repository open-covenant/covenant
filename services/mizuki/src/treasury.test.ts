import { describe, expect, it } from 'vitest';
import {
  serviceDependencies,
  type RefundProtectionEvidence,
  type ServiceReadinessReport,
} from './readiness.js';
import { MemoryStore } from './store.js';
import { treasurySnapshot } from './treasury.js';
import type { Quote } from './types.js';

const quote: Quote = {
  id: '00000000-0000-4000-8000-000000000001',
  issueUrl: 'https://github.com/example/tool/issues/1',
  owner: 'example',
  repo: 'tool',
  issueNumber: 1,
  issueTitle: 'Fix a bounded issue',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: ['pnpm test'],
  expiresAt: '2099-01-01T00:00:00.000Z',
};

const protection: RefundProtectionEvidence = {
  refundTreasury: 'refund-treasury',
  refundMint: 'usdc-mint',
  refundDecimals: 6,
  finalizedBalanceRaw: '100000000',
  pendingRefundRaw: '2000000',
  treasuryAvailableRefundRaw: '98000000',
  remainingRefundLimitUsdCents: 9_800,
  availableRefundRaw: '98000000',
  escrowAuthority: 'escrow-authority',
  finalizedEscrowBalanceLamports: '2000000000',
  availableEscrowReserveLamports: '1900000000',
};

describe('treasury snapshot', () => {
  it('never presents ledger allocations as custody or protection', async () => {
    const store = new MemoryStore();
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'unverified-deposit',
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: 100,
    });

    const snapshot = await treasurySnapshot(store);

    expect(snapshot).toMatchObject({
      recordedNetFlowUsd: 100,
      refundProtection: {
        status: 'unavailable',
        finalizedBalanceAtomic: null,
        liabilitiesBacked: null,
      },
      allocationModel: {
        source: 'application_ledger',
        custodyVerified: false,
        targetsSatisfied: true,
      },
    });
  });

  it('publishes exact signer custody only when liabilities reconcile', async () => {
    const store = await storeWithOutstandingLiability();

    const snapshot = await treasurySnapshot(store, readiness(protection));

    expect(snapshot.refundProtection).toEqual({
      status: 'verified',
      source: 'policy_signer_finalized',
      refundTreasury: 'refund-treasury',
      refundMint: 'usdc-mint',
      refundDecimals: 6,
      finalizedBalanceAtomic: '100000000',
      signerOutstandingLiabilityAtomic: '2000000',
      unencumberedBalanceAtomic: '98000000',
      newIntakeCapacityAtomic: '98000000',
      remainingDailyLimitUsdCents: 9_800,
      localOutstandingLiabilityAtomic: '2000000',
      liabilityReconciled: true,
      liabilitiesBacked: true,
      checkedAt: '2026-08-22T12:00:00.000Z',
    });
  });

  it('degrades protection when signer and application liabilities differ', async () => {
    const store = await storeWithOutstandingLiability();
    const mismatched = {
      ...protection,
      pendingRefundRaw: '3000000',
      treasuryAvailableRefundRaw: '97000000',
      availableRefundRaw: '97000000',
    };

    expect((await treasurySnapshot(store, readiness(mismatched))).refundProtection).toMatchObject({
      status: 'degraded',
      signerOutstandingLiabilityAtomic: '3000000',
      localOutstandingLiabilityAtomic: '2000000',
      liabilityReconciled: false,
      liabilitiesBacked: true,
    });
  });

  it('does not let off-wallet cost rows change verified custody', async () => {
    const store = await storeWithOutstandingLiability();
    const before = await treasurySnapshot(store, readiness(protection));
    await store.appendLedger({
      kind: 'route_cost',
      referenceId: 'off-wallet-provider-cost',
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: 7.5,
    });
    const after = await treasurySnapshot(store, readiness(protection));

    expect(after.recordedNetFlowUsd).toBe(before.recordedNetFlowUsd - 7.5);
    expect(after.refundProtection).toEqual(before.refundProtection);
  });

  it('never treats SOL bounty custody as a debit or credit in the USD allocation model', async () => {
    const store = new MemoryStore();
    await store.appendLedger({
      kind: 'bounty_reserved',
      referenceId: 'bounty-1',
      asset: 'SOL',
      amountAtomic: '100000000',
      amountUsd: 20,
      transaction: 'funding-signature',
    });
    await store.appendLedger({
      kind: 'bounty_returned',
      referenceId: 'bounty-1',
      asset: 'SOL',
      amountAtomic: '100000000',
      amountUsd: 20,
      transaction: 'refund-signature',
    });

    expect(await treasurySnapshot(store)).toMatchObject({
      recordedNetFlowUsd: 0,
      allocationModel: {
        plannedImprovementAllocationUsd: 0,
        plannedResearchAllocationUsd: 0,
      },
    });
  });

  it('keeps a delivered payment recorded as a liability until signer discharge', async () => {
    const store = await storeWithOutstandingLiability();
    const [delivered] = await store.jobsList();

    expect((await treasurySnapshot(store)).localOutstandingLiabilityUsd).toBe(2);

    await store.patchJob(delivered!.id, {
      refundLiabilityDischargedAt: '2026-08-22T12:00:00.000Z',
      refundLiabilityDischargeEvidenceHash: 'e'.repeat(64),
    });
    expect((await treasurySnapshot(store)).localOutstandingLiabilityUsd).toBe(0);
  });
});

async function storeWithOutstandingLiability(): Promise<MemoryStore> {
  const store = new MemoryStore();
  const { job } = await store.createJob(
    quote,
    { payer: 'customer', transaction: 'settlement-signature', amountAtomic: '2000000' },
    'payment-1',
  );
  await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
    refundLiabilityId: 'liability-1',
  });
  return store;
}

function readiness(evidence: RefundProtectionEvidence): ServiceReadinessReport {
  const checkedAt = '2026-08-22T12:00:00.000Z';
  const dependencies = Object.fromEntries(
    serviceDependencies.map((name) => [
      name,
      {
        ok: true,
        checkedAt,
        latencyMs: 1,
        ...(name === 'policy_signer' ? { refundProtection: evidence } : {}),
      },
    ]),
  ) as ServiceReadinessReport['dependencies'];
  return {
    ready: true,
    checkedAt,
    ageMs: 0,
    lastSuccessfulAt: checkedAt,
    lastSuccessfulAgeMs: 0,
    dependencies,
    failed: [],
  };
}
