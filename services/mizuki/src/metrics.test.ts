import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { createRescueBounty, transitionRescueBounty } from './domain/index.js';
import { metrics, prometheus } from './metrics.js';
import { MemoryStore } from './store.js';
import type { GithubAuthorizationReceipt, Quote } from './types.js';

const authorization = (actorId: string): GithubAuthorizationReceipt => ({
  label: 'mizuki:authorized',
  actorId,
  actorLogin: `maintainer-${actorId}`,
  permission: 'maintain',
  authorizedAt: '2026-08-22T10:00:00.000Z',
  verifiedAt: '2026-08-22T10:01:00.000Z',
  evidenceHash: 'e'.repeat(64),
});

function quote(id: string, owner: string, actorId?: string): Quote {
  return {
    id,
    issueUrl: `https://github.com/${owner}/tool/issues/1`,
    owner,
    repo: 'tool',
    issueNumber: 1,
    issueTitle: 'Fix a bounded issue',
    issueBody: '',
    baseSha: 'a'.repeat(40),
    defaultBranch: 'main',
    ...(actorId ? { installationId: 42, authorizationReceipt: authorization(actorId) } : {}),
    class: 'micro',
    priceAtomic: '2000000',
    maxFiles: 3,
    maxCostUsd: 0.8,
    validationCommands: ['pnpm test'],
    expiresAt: '2099-01-01T00:00:00.000Z',
  };
}

describe('metrics', () => {
  it('publishes native creator fees without mixing them into the USDC treasury', async () => {
    const store = new MemoryStore();
    const inputs = [
      quote('00000000-0000-4000-8000-000000000001', 'external-one', 'github-1'),
      quote('00000000-0000-4000-8000-000000000002', 'external-two', 'github-1'),
      quote('00000000-0000-4000-8000-000000000003', 'internal', 'github-2'),
      quote('00000000-0000-4000-8000-000000000004', 'unverified'),
    ];

    for (const [index, value] of inputs.entries()) {
      const { job } = await store.createJob(
        value,
        {
          payer: `payer-${index}`,
          transaction: `transaction-${index}`,
          amountAtomic: value.priceAtomic,
        },
        `payment-${index}`,
      );
      await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
        estimatedCostUsd: 0.5,
        refundLiabilityId: `liability-${index}`,
      });
      await store.patchJob(job.id, {
        refundLiabilityDischargedAt: '2026-08-22T12:00:00.000Z',
        refundLiabilityDischargeEvidenceHash: 'e'.repeat(64),
      });
    }
    await store.appendLedger({
      kind: 'creator_fee',
      referenceId: 'clawpump:mizuki-agent:1000000000',
      asset: 'SOL',
      amountAtomic: '1000000000',
      amountUsd: 999,
      transaction: 'creator-fee-transaction',
    });
    await store.appendLedger({
      kind: 'creator_fee',
      referenceId: 'clawpump:mizuki-agent:1250000000',
      asset: 'SOL',
      amountAtomic: '250000000',
      amountUsd: 0,
      transaction: 'creator-fee-transaction-2',
    });
    await store.appendLedger({
      kind: 'creator_fee',
      referenceId: 'clawpump:mizuki-agent:wrong-asset-fee',
      asset: 'USDC',
      amountAtomic: '9000000',
      amountUsd: 9,
    });

    const value = await metrics(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_INTERNAL_REPOS: 'internal/tool',
        CLAWPUMP_AGENT_ID: 'mizuki-agent',
      }),
      store,
    );

    expect(value).toMatchObject({
      paidJobs: 4,
      deliveredPrs: 4,
      externalRepositories: 2,
      externalMaintainers: 1,
      settledCustomerReceiptsUsd: 8,
      recognizedRevenueUsd: 8,
      platformReportedCreatorFeesSentLamports: '1250000000',
      refundSuccessRate: null,
      variableRouteCostEstimateUsd: 2,
      recognizedRevenueLessVariableRouteEstimateUsd: 6,
      grossMarginStatus: 'unverified',
      costCoverage: {
        included: [
          'gateway_model_token_rate_estimate',
          'gateway_sandbox_runtime_estimate',
          'reviewer_model_token_rate_estimate',
        ],
        excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
      },
      refundProtection: { status: 'unavailable' },
      plannedImprovementAllocationUsd: 0,
    });
    expect(prometheus(value)).toContain('mizuki_external_maintainers 1');
    expect(prometheus(value)).toContain(
      'mizuki_platform_reported_creator_fees_sent_lamports 1250000000',
    );
    expect(prometheus(value)).toContain('mizuki_refund_success_ratio NaN');
    expect(prometheus(value)).toContain('mizuki_gross_margin_verified 0');
    expect(prometheus(value)).not.toContain('mizuki_gross_margin_usd');
    expect(prometheus(value)).not.toContain('mizuki_route_cost_usd');
    expect(prometheus(value)).not.toContain('mizuki_creator_fees_usd');
    expect(prometheus(value)).not.toContain('mizuki_refund_reserve_usd');
    expect(prometheus(value)).not.toContain('mizuki_capability_pool_usd');
  });

  it('recognizes revenue only after the refund liability is discharged', async () => {
    const store = new MemoryStore();
    const value = quote('00000000-0000-4000-8000-000000000021', 'recognition');
    const { job } = await store.createJob(
      value,
      { payer: 'payer', transaction: 'transaction', amountAtomic: '2000000' },
      'payment',
    );
    await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      refundLiabilityId: 'liability',
      estimatedCostUsd: 0.5,
    });

    await expect(
      metrics(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }), store),
    ).resolves.toMatchObject({
      settledCustomerReceiptsUsd: 2,
      recognizedRevenueUsd: 0,
      recognizedRevenueLessVariableRouteEstimateUsd: -0.5,
    });

    await store.patchJob(job.id, {
      refundLiabilityDischargedAt: '2026-08-22T12:00:00.000Z',
      refundLiabilityDischargeEvidenceHash: 'e'.repeat(64),
    });
    await expect(
      metrics(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }), store),
    ).resolves.toMatchObject({
      recognizedRevenueUsd: 2,
      recognizedRevenueLessVariableRouteEstimateUsd: 1.5,
    });
  });

  it('counts unresolved failures as outstanding refund obligations', async () => {
    const store = new MemoryStore();
    const failedQuote = quote('00000000-0000-4000-8000-000000000011', 'failed');
    const refundedQuote = quote('00000000-0000-4000-8000-000000000012', 'refunded');
    const { job: failed } = await store.createJob(
      failedQuote,
      { payer: 'payer-failed', transaction: 'transaction-failed', amountAtomic: '2000000' },
      'payment-failed',
    );
    const { job: refunded } = await store.createJob(
      refundedQuote,
      { payer: 'payer-refunded', transaction: 'transaction-refunded', amountAtomic: '2000000' },
      'payment-refunded',
    );
    await store.transitionJob(failed.id, 'settlement_pending', 'failed');
    await store.transitionJob(refunded.id, 'settlement_pending', 'refunded');

    const value = await metrics(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }), store);

    expect(value).toMatchObject({
      refundCount: 1,
      refundPending: 1,
      refundSuccessRate: 0.5,
    });
    expect(prometheus(value)).toContain('mizuki_refund_success_ratio 0.5');
  });

  it('publishes the age of unresolved settlement and refund states', async () => {
    vi.useFakeTimers();
    try {
      vi.setSystemTime(new Date('2026-08-22T10:00:00.000Z'));
      const store = new MemoryStore();
      const pendingQuote = quote('00000000-0000-4000-8000-000000000031', 'pending');
      const failedQuote = quote('00000000-0000-4000-8000-000000000032', 'failed');
      await store.createJob(
        pendingQuote,
        { payer: 'payer-pending', transaction: 'pending', amountAtomic: '2000000' },
        'payment-pending',
      );
      const { job: failed } = await store.createJob(
        failedQuote,
        { payer: 'payer-failed', transaction: 'transaction-failed', amountAtomic: '2000000' },
        'payment-failed',
      );
      await store.transitionJob(failed.id, 'settlement_pending', 'failed');

      vi.advanceTimersByTime(90_000);
      const value = await metrics(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }), store);

      expect(value).toMatchObject({
        settlementPending: 1,
        settlementPendingOldestSeconds: 90,
        refundPending: 1,
        refundPendingOldestSeconds: 90,
      });
      expect(prometheus(value)).toContain('mizuki_settlement_pending_oldest_seconds 90');
      expect(prometheus(value)).toContain('mizuki_refund_pending_oldest_seconds 90');
    } finally {
      vi.useRealTimers();
    }
  });

  it('exposes an open bounty without finalized funding as a stop condition', async () => {
    const store = new MemoryStore();
    const draft = createRescueBounty({
      id: 'bounty-unfunded',
      sourceJobId: 'job-unfunded',
      failureReceiptId: 'failure-unfunded',
      repository: 'open-covenant/covenant',
      issueNumber: 42,
      issueUrl: 'https://github.com/open-covenant/covenant/issues/42',
      jobPriceCents: 200,
      at: '2026-08-22T10:00:00.000Z',
    });
    const funding = transitionRescueBounty(draft, 'funding', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: draft.revision,
    });
    const open = transitionRescueBounty(funding, 'open', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: funding.revision,
    });
    await store.createBounty(open);

    const value = await metrics(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }), store);

    expect(value).toMatchObject({ bountiesOpen: 1, bountiesUnfundedOpen: 1 });
    expect(prometheus(value)).toContain('mizuki_bounties_unfunded_open 1');
  });
});
