import { describe, expect, it } from 'vitest';
import { createRescueBounty } from './domain/index.js';
import { publicBounty, publicJob, publicTreasury } from './public-api.js';
import { MemoryStore } from './store.js';
import type { ProviderRouteReceipt, Quote } from './types.js';

const quote: Quote = {
  id: '00000000-0000-4000-8000-000000000021',
  issueUrl: 'https://github.com/public/tool/issues/1',
  owner: 'public',
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

describe('public accounting', () => {
  it('identifies job cost as a partial variable execution estimate', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment', amountAtomic: quote.priceAtomic },
      'payment-key',
    );
    const delivered = await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      estimatedCostUsd: 0.42,
    });

    const receipt = publicJob(delivered);

    expect(receipt).toMatchObject({
      variableRouteCostEstimateUsd: 0.42,
      costCoverage: {
        included: [
          'gateway_model_token_rate_estimate',
          'gateway_sandbox_runtime_estimate',
          'reviewer_model_token_rate_estimate',
        ],
        excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
      },
    });
    expect(receipt).not.toHaveProperty('estimatedCostUsd');
  });

  it('does not expose upstream failure bodies in public job receipts', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-redaction', amountAtomic: quote.priceAtomic },
      'payment-redaction-key',
    );
    const failed = await store.transitionJob(job.id, 'settlement_pending', 'failed', {
      error: 'UsePod returned 500: secret upstream diagnostic',
    });

    expect(publicJob(failed).error).toBe('The execution route did not complete reliably.');
    expect(JSON.stringify(publicJob(failed))).not.toContain('secret upstream diagnostic');
  });

  it('publishes completed review work on refunded jobs without exposing provider balances', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-reviewed-refund', amountAtomic: quote.priceAtomic },
      'payment-reviewed-refund-key',
    );
    const provider = {
      model: 'review-model',
      route: 'marketplace',
      providerId: 'provider-7',
      requestId: 'request-9',
      costMicrounits: '175000',
      balanceRemaining: '4000000',
      apiToken: 'upstream-secret',
    } as ProviderRouteReceipt;
    await store.patchJob(job.id, {
      reviewReceipt: {
        approved: true,
        reason: 'The bounded patch passed independent review.',
        reviewedAt: '2026-08-23T10:00:00.000Z',
        artifactHash: 'b'.repeat(64),
        provider,
      },
    });
    const refunded = await store.transitionJob(job.id, 'settlement_pending', 'refunded', {
      error: 'GitHub returned an upstream secret after review',
      refundTransaction: 'refund-reviewed-job',
    });

    const receipt = publicJob(refunded);

    expect(receipt).toMatchObject({
      state: 'refunded',
      refundTransaction: 'refund-reviewed-job',
      review: {
        approved: true,
        reason: 'The bounded patch passed independent review.',
        reviewedAt: '2026-08-23T10:00:00.000Z',
        artifactHash: 'b'.repeat(64),
        provider: {
          model: 'review-model',
          route: 'marketplace',
          providerId: 'provider-7',
          requestId: 'request-9',
          costMicrounits: '175000',
        },
      },
    });
    expect(JSON.stringify(receipt)).not.toContain('balanceRemaining');
    expect(JSON.stringify(receipt)).not.toContain('upstream-secret');
  });

  it('publishes failed and rejected review attempts without exposing internal errors', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-review-attempts', amountAtomic: quote.priceAtomic },
      'payment-review-attempts-key',
    );
    const provider = {
      model: 'review-model',
      route: 'marketplace',
      providerId: 'provider-7',
      requestId: 'request-9',
      costMicrounits: '175000',
      balanceRemaining: '4000000',
      rawResponse: 'private-provider-response',
    } as ProviderRouteReceipt;
    const rejectedAt = '2026-08-23T10:00:00.000Z';
    const failedAt = '2026-08-23T10:01:00.000Z';
    const updated = await store.patchJob(job.id, {
      reviewAttempts: [
        {
          id: 'review-rejected',
          phase: 'implementation',
          status: 'completed',
          artifactHash: 'a'.repeat(64),
          reviewedAt: rejectedAt,
          costUsd: 0.0175,
          provider,
          approved: false,
          reason: 'The patch does not cover the reported edge case.',
        },
        {
          id: 'review-failed',
          phase: 'repair',
          status: 'failed',
          artifactHash: 'b'.repeat(64),
          reviewedAt: failedAt,
          costUsd: 0.12,
          error: 'UsePod returned 500: secret upstream diagnostic',
        },
      ],
    });

    const receipt = publicJob(updated);

    expect(receipt.reviewAttempts).toEqual([
      {
        phase: 'implementation',
        status: 'completed',
        artifactHash: 'a'.repeat(64),
        reviewedAt: rejectedAt,
        costUsd: 0.0175,
        provider: {
          model: 'review-model',
          route: 'marketplace',
          providerId: 'provider-7',
          requestId: 'request-9',
          costMicrounits: '175000',
        },
        approved: false,
        reason: 'The patch does not cover the reported edge case.',
      },
      {
        phase: 'repair',
        status: 'failed',
        artifactHash: 'b'.repeat(64),
        reviewedAt: failedAt,
        costUsd: 0.12,
        reason: 'The independent review did not complete reliably.',
      },
    ]);
    expect(JSON.stringify(receipt)).not.toContain('secret upstream diagnostic');
    expect(JSON.stringify(receipt)).not.toContain('private-provider-response');
    expect(JSON.stringify(receipt)).not.toContain('balanceRemaining');
  });

  it('publishes bounty review commitments and a whitelisted provider receipt', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-bounty-review', amountAtomic: quote.priceAtomic },
      'payment-bounty-review-key',
    );
    const provider = {
      model: 'bounty-review-model',
      route: 'marketplace',
      providerId: 'provider-11',
      requestId: 'request-12',
      costMicrounits: '90000',
      balanceRemaining: '3910000',
      rawResponse: 'private-upstream-body',
    } as ProviderRouteReceipt;
    const bounty = {
      ...createRescueBounty({
        id: 'bounty-review-receipt',
        sourceJobId: job.id,
        failureReceiptId: 'failure-review-receipt',
        repository: 'public/tool',
        issueNumber: 1,
        issueUrl: quote.issueUrl,
        jobPriceCents: 200,
        at: '2026-08-23T09:00:00.000Z',
      }),
      validationReceipt: {
        id: 'review-receipt',
        approved: true,
        reason: 'Repository checks and the bounded patch passed review.',
        reviewedAt: '2026-08-23T11:00:00.000Z',
        headSha: 'c'.repeat(40),
        baseSha: 'd'.repeat(40),
        baseRef: 'main',
        diffHash: 'e'.repeat(64),
        provider,
      },
    };

    const receipt = await publicBounty(store, bounty);

    expect(receipt.review).toEqual({
      approved: true,
      reason: 'Repository checks and the bounded patch passed review.',
      reviewedAt: '2026-08-23T11:00:00.000Z',
      headSha: 'c'.repeat(40),
      baseSha: 'd'.repeat(40),
      baseRef: 'main',
      diffHash: 'e'.repeat(64),
      provider: {
        model: 'bounty-review-model',
        route: 'marketplace',
        providerId: 'provider-11',
        requestId: 'request-12',
        costMicrounits: '90000',
      },
    });
    expect(JSON.stringify(receipt)).not.toContain('balanceRemaining');
    expect(JSON.stringify(receipt)).not.toContain('private-upstream-body');
  });

  it('publishes native creator fees and distinguishes estimated from recorded costs', async () => {
    const store = new MemoryStore();
    await store.appendLedger({
      kind: 'creator_fee',
      referenceId: 'creator-fee',
      asset: 'SOL',
      amountAtomic: '1250000000',
      amountUsd: 0,
      transaction: 'creator-fee-transaction',
    });
    await store.appendLedger({
      kind: 'route_cost',
      referenceId: 'route-cost',
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: 0.25,
    });
    await store.appendLedger({
      kind: 'operating_cost',
      referenceId: 'operating-cost',
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: 1,
    });

    const treasury = await publicTreasury(store);
    const creatorFee = treasury.ledger.find(
      (entry) => entry.type === 'platform_reported_creator_fee',
    );
    const routeCost = treasury.ledger.find((entry) => entry.type === 'route_cost');
    const operatingCost = treasury.ledger.find((entry) => entry.type === 'operating_cost');

    expect(treasury).toMatchObject({
      refundProtection: { status: 'unavailable', finalizedBalanceAtomic: null },
      allocationModel: {
        source: 'application_ledger',
        custodyVerified: false,
      },
    });
    expect(treasury).not.toHaveProperty('totalUsd');
    expect(treasury).not.toHaveProperty('reserveHealthy');
    expect(treasury.allocationModel.buckets[0]).not.toHaveProperty('balanceUsd');
    expect(treasury.allocationModel.buckets[0]).not.toHaveProperty('availableUsd');

    expect(creatorFee).toMatchObject({
      description: 'ClawPump-reported creator fee distribution (native SOL)',
      direction: 'allocation',
      amountAtomic: '1250000000',
      asset: 'SOL',
    });
    expect(creatorFee).not.toHaveProperty('amountUsd');
    expect(routeCost).toMatchObject({
      description: 'Variable execution cost estimate',
      amountUsd: 0.25,
    });
    expect(operatingCost).toMatchObject({
      description: 'Recorded operating cost',
      amountUsd: 1,
    });
  });
});
