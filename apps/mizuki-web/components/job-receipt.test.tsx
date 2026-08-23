import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { Job, ProviderRouteReceipt } from '@/lib/types';
import { JobReceipt } from './job-receipt';

describe('JobReceipt', () => {
  it('shows provider work on a refunded job without exposing the funded balance', () => {
    const provider = {
      model: 'review-model',
      route: 'marketplace',
      providerId: 'provider-7',
      requestId: 'request-9',
      costMicrounits: '175000',
      balanceRemaining: '4000000',
    } as ProviderRouteReceipt;
    const job: Job = {
      id: 'job-reviewed-refund',
      state: 'refunded',
      issueUrl: 'https://github.com/public/tool/issues/1',
      class: 'micro',
      priceAtomic: '2000000',
      paymentTransaction: 'payment-reviewed-refund',
      refundTransaction: 'refund-reviewed-job',
      error: 'The validated patch could not be delivered to GitHub.',
      review: {
        approved: true,
        reason: 'The bounded patch passed independent review.',
        reviewedAt: '2026-08-23T10:00:00.000Z',
        artifactHash: 'b'.repeat(64),
        provider,
      },
      reviewAttempts: [
        {
          phase: 'implementation',
          status: 'completed',
          artifactHash: 'c'.repeat(64),
          reviewedAt: '2026-08-23T09:55:00.000Z',
          costUsd: 0.0175,
          provider,
          approved: false,
          reason: 'The first patch missed the reported edge case.',
        },
        {
          phase: 'repair',
          status: 'failed',
          artifactHash: 'd'.repeat(64),
          reviewedAt: '2026-08-23T10:00:00.000Z',
          costUsd: 0.12,
          reason: 'The independent review did not complete reliably.',
        },
      ],
      changedFiles: ['src/fix.ts'],
      validations: [{ command: 'pnpm test', exitCode: 0 }],
      variableRouteCostEstimateUsd: 0.42,
      costCoverage: {
        included: [
          'gateway_model_token_rate_estimate',
          'gateway_sandbox_runtime_estimate',
          'reviewer_model_token_rate_estimate',
        ],
        excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
      },
      createdAt: '2026-08-23T09:00:00.000Z',
      updatedAt: '2026-08-23T10:05:00.000Z',
    };

    const html = renderToStaticMarkup(<JobReceipt initial={job} live={false} />);

    expect(html).toContain('Independent review receipt');
    expect(html).toContain('review-model');
    expect(html).toContain('provider-7');
    expect(html).toContain('request-9');
    expect(html).toContain('175000 microunits');
    expect(html).toContain('records provider work, not a successful delivery');
    expect(html).toContain('b'.repeat(64));
    expect(html).toContain('Review attempt ledger');
    expect(html).toContain('rejected');
    expect(html).toContain('failed');
    expect(html).toContain('The first patch missed the reported edge case.');
    expect(html).toContain('The independent review did not complete reliably.');
    expect(html).toContain('c'.repeat(64));
    expect(html).toContain('d'.repeat(64));
    expect(html).not.toContain('4000000');
    expect(html).not.toContain('balanceRemaining');
  });
});
