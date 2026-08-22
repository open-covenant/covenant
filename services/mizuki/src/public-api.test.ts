import { describe, expect, it } from 'vitest';
import { publicJob, publicTreasury } from './public-api.js';
import { MemoryStore } from './store.js';
import type { Quote } from './types.js';

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
