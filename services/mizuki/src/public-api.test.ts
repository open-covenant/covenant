import { describe, expect, it } from 'vitest';
import {
  createContributorEscrow,
  createRescueBounty,
  transitionContributorEscrow,
  transitionRescueBounty,
} from './domain/index.js';
import {
  isPublicBounty,
  publicActivityFeed,
  publicBounty,
  publicJob,
  publicTreasury,
} from './public-api.js';
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
  it('publishes no bounty record or activity before escrow funding finalizes', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-public-boundary', amountAtomic: quote.priceAtomic },
      'payment-public-boundary-key',
    );
    const draft = createRescueBounty({
      id: 'bounty-public-boundary',
      sourceJobId: job.id,
      failureReceiptId: 'failure-public-boundary',
      repository: 'public/tool',
      issueNumber: 1,
      issueUrl: quote.issueUrl,
      jobPriceCents: 200,
      at: '2026-08-23T09:00:00.000Z',
    });
    await store.createBounty(draft);
    await store.appendActivity('bounty.created', draft.id, {});
    await store.appendActivity('bounty.funded', draft.id, { transaction: 'premature' });

    expect(await isPublicBounty(store, draft)).toBe(false);
    expect(await publicActivityFeed(store)).toEqual([]);

    const funding = transitionRescueBounty(draft, 'funding', {
      at: '2026-08-23T09:01:00.000Z',
      expectedRevision: draft.revision,
    });
    await store.updateBounty(funding, draft.revision);
    const opened = transitionRescueBounty(funding, 'open', {
      at: '2026-08-23T09:03:00.000Z',
      expectedRevision: funding.revision,
    });
    await store.updateBounty(opened, funding.revision);

    let escrow = createContributorEscrow({
      id: 'escrow-public-boundary',
      bountyId: draft.id,
      repository: 'public/tool',
      issueNumber: 1,
      issueTitle: 'Fix a bounded issue',
      issueBody: '',
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reviewPolicy: { version: 1, model: 'review-model', maxFiles: 3 },
      amountCents: 1_000,
      acceptanceHash: 'b'.repeat(64),
      expiresAt: '2026-08-30T09:00:00.000Z',
      at: '2026-08-23T09:00:00.000Z',
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'funding', {
      at: '2026-08-23T09:01:00.000Z',
      expectedRevision: escrow.revision,
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'funded', {
      at: '2026-08-23T09:02:00.000Z',
      expectedRevision: escrow.revision,
      transactionSignature: 'funding-signature',
      reservationId: 'reservation-public-boundary',
      amountAtomic: '50000000',
    });
    await store.saveEscrow(escrow);
    await store.appendActivity('bounty.funded', draft.id, {
      transaction: 'funding-signature',
    });

    expect(await isPublicBounty(store, opened)).toBe(false);
    expect(await publicActivityFeed(store)).toEqual([]);

    await store.transitionJob(job.id, 'settlement_pending', 'refunded', {
      error: 'The patch did not pass repository checks.',
      refundTransaction: 'customer-refund-public-boundary',
    });

    expect(await isPublicBounty(store, opened)).toBe(true);
    expect(await publicActivityFeed(store)).toEqual([
      expect.objectContaining({
        kind: 'bounty_funded',
        title: 'Funded bounty published',
        transaction: 'funding-signature',
      }),
    ]);
  });

  it('keeps the customer refund and bounty escrow return as separate transactions', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'customer-payment', amountAtomic: quote.priceAtomic },
      'customer-payment-key',
    );
    await store.transitionJob(job.id, 'settlement_pending', 'refunded', {
      error: 'The patch did not pass repository checks.',
      refundTransaction: 'customer-usdc-refund',
    });
    const bounty = createRescueBounty({
      id: 'bounty-split-refunds',
      sourceJobId: job.id,
      failureReceiptId: 'failure-split-refunds',
      repository: 'public/tool',
      issueNumber: 1,
      issueUrl: quote.issueUrl,
      jobPriceCents: 200,
      at: '2026-08-23T09:00:00.000Z',
    });
    let escrow = createContributorEscrow({
      id: 'escrow-split-refunds',
      bountyId: bounty.id,
      repository: 'public/tool',
      issueNumber: 1,
      issueTitle: quote.issueTitle,
      issueBody: '',
      baseRef: 'main',
      baseSha: 'a'.repeat(40),
      reviewPolicy: { version: 1, model: 'review-model', maxFiles: 3 },
      amountCents: 1_000,
      acceptanceHash: 'b'.repeat(64),
      expiresAt: '2026-08-30T09:00:00.000Z',
      at: '2026-08-23T09:00:00.000Z',
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'funding', {
      at: '2026-08-23T09:01:00.000Z',
      expectedRevision: escrow.revision,
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'funded', {
      at: '2026-08-23T09:02:00.000Z',
      expectedRevision: escrow.revision,
      transactionSignature: 'bounty-sol-funding',
      reservationId: 'reservation-split-refunds',
      amountAtomic: '50000000',
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'refund_pending', {
      at: '2026-08-23T09:03:00.000Z',
      expectedRevision: escrow.revision,
    });
    escrow = await store.saveEscrow(escrow);
    escrow = transitionContributorEscrow(escrow, 'refunded', {
      at: '2026-08-23T09:04:00.000Z',
      expectedRevision: escrow.revision,
      transactionSignature: 'bounty-sol-return',
    });
    await store.saveEscrow(escrow);

    await expect(publicBounty(store, bounty)).resolves.toMatchObject({
      customerRefundTransaction: 'customer-usdc-refund',
      escrowTransaction: 'bounty-sol-funding',
      escrowReturnTransaction: 'bounty-sol-return',
    });
  });

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
      issueTitle: 'Fix a bounded issue',
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

  it('publishes exact delivery and refund-liability discharge commitments', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'payment-evidence', amountAtomic: quote.priceAtomic },
      'payment-evidence-key',
    );
    const delivered = await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      prUrl: 'https://github.com/public/tool/pull/7',
      mergedAt: '2026-08-23T11:00:00.000Z',
      deliveryEvidence: {
        pullRequestNumber: 7,
        headSha: 'b'.repeat(40),
        baseSha: 'a'.repeat(40),
        baseRef: 'main',
        diffHash: 'c'.repeat(64),
        observedAt: '2026-08-23T10:00:00.000Z',
      },
      refundLiabilityDischargedAt: '2026-08-23T11:00:03.000Z',
      refundLiabilityDischargeEvidenceHash: 'd'.repeat(64),
      refundLiabilityId: 'private-liability-id',
    });

    const receipt = publicJob(delivered);

    expect(receipt).toMatchObject({
      mergedAt: '2026-08-23T11:00:00.000Z',
      deliveryEvidence: {
        pullRequestNumber: 7,
        headSha: 'b'.repeat(40),
        baseSha: 'a'.repeat(40),
        baseRef: 'main',
        diffHash: 'c'.repeat(64),
        observedAt: '2026-08-23T10:00:00.000Z',
      },
      refundLiabilityDischarge: {
        dischargedAt: '2026-08-23T11:00:03.000Z',
        evidenceHash: 'd'.repeat(64),
      },
    });
    expect(receipt).not.toHaveProperty('refundLiabilityId');
    expect(JSON.stringify(receipt)).not.toContain('private-liability-id');
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

    expect(publicJob(failed).error).toBe('A required AI service did not complete the work.');
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
        reason: 'The bounded patch passed internal review.',
        reviewedAt: '2026-08-23T10:00:00.000Z',
        artifactHash: 'b'.repeat(64),
        provider,
      },
    });
    const refunded = await store.transitionJob(job.id, 'settlement_pending', 'refunded', {
      error: 'GitHub returned an upstream secret after review',
      refundTransaction: 'refund-reviewed-job',
    });

    const receipt = publicJob({ ...refunded, refundOperationId: 'private-refund-operation' });

    expect(receipt).toMatchObject({
      state: 'refunded',
      refundTransaction: 'refund-reviewed-job',
      review: {
        approved: true,
        reason:
          'The separate AI review approved the patch against the issue scope and repository checks.',
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
    expect(JSON.stringify(receipt)).not.toContain('passed internal review');
    expect(JSON.stringify(receipt)).not.toContain('private-refund-operation');
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
          attemptNumber: 1,
          maxAttempts: 2,
          maxCostUsd: 0.06,
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
        attemptNumber: 1,
        maxAttempts: 2,
        maxCostUsd: 0.06,
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
        reason:
          'The separate AI review did not approve the patch against the issue scope and repository checks.',
      },
      {
        phase: 'repair',
        status: 'failed',
        artifactHash: 'b'.repeat(64),
        reviewedAt: failedAt,
        costUsd: 0.12,
        reason: 'The separate AI review could not be completed.',
      },
    ]);
    expect(JSON.stringify(receipt)).not.toContain('secret upstream diagnostic');
    expect(JSON.stringify(receipt)).not.toContain('private-provider-response');
    expect(JSON.stringify(receipt)).not.toContain('balanceRemaining');
    expect(JSON.stringify(receipt)).not.toContain('reported edge case');
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
      dispute: {
        id: 'dispute-public-receipt',
        claimantId: 'contributor-private-id',
        reason: 'private claimant narrative',
        state: 'refunded',
        openedAt: '2026-08-23T11:05:00.000Z',
        resolution: {
          id: 'resolution-public-receipt',
          idempotencyKey: 'private-idempotency-key',
          requestedDecision: 'release',
          settlementDecision: 'refund',
          evidence: {
            summary: 'The exact reviewed commit was not merged before the claim deadline.',
            references: ['https://github.com/public/tool/pull/7'],
          },
          evidenceHash: 'f'.repeat(64),
          decidedAt: '2026-08-23T11:10:00.000Z',
          resolvedAt: '2026-08-23T11:12:00.000Z',
          transactionSignature: 'dispute-sol-return',
        },
      },
    };

    const receipt = await publicBounty(store, bounty);

    expect(receipt.review).toEqual({
      approved: true,
      reason:
        'The separate AI review approved the patch against the issue scope and repository checks.',
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
    expect(JSON.stringify(receipt)).not.toContain('bounded patch passed review');
    expect(receipt.dispute?.resolution).toEqual({
      requestedDecision: 'release',
      settlementDecision: 'refund',
      summary: 'The exact reviewed commit was not merged before the claim deadline.',
      references: ['https://github.com/public/tool/pull/7'],
      evidenceHash: 'f'.repeat(64),
      decidedAt: '2026-08-23T11:10:00.000Z',
      resolvedAt: '2026-08-23T11:12:00.000Z',
      transactionSignature: 'dispute-sol-return',
    });
    expect(JSON.stringify(receipt)).not.toContain('private claimant narrative');
    expect(JSON.stringify(receipt)).not.toContain('private-idempotency-key');
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
      description: 'Tracked model and sandbox cost estimate',
      amountUsd: 0.25,
    });
    expect(operatingCost).toMatchObject({
      description: 'Recorded operating cost',
      amountUsd: 1,
    });
  });
});
