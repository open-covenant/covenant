import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { Job, ProviderRouteReceipt } from '@/lib/types';
import { JobReceipt } from './job-receipt';

describe('JobReceipt', () => {
  it('shows a merged delivery and the discharged refund-liability commitment', () => {
    const job: Job = {
      id: 'job-merged',
      state: 'delivered',
      issueUrl: 'https://github.com/public/tool/issues/1',
      class: 'micro',
      priceAtomic: '2000000',
      paymentTransaction: 'payment-merged',
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
      refundLiabilityDischarge: {
        dischargedAt: '2026-08-23T11:00:03.000Z',
        evidenceHash: 'd'.repeat(64),
      },
      changedFiles: ['README.md'],
      validations: [{ command: 'prettier --check README.md', exitCode: 0 }],
      variableRouteCostEstimateUsd: 0.2,
      costCoverage: {
        included: [
          'gateway_model_token_rate_estimate',
          'gateway_sandbox_runtime_estimate',
          'reviewer_model_token_rate_estimate',
        ],
        excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
      },
      createdAt: '2026-08-23T09:00:00.000Z',
      updatedAt: '2026-08-23T11:00:03.000Z',
    };

    const html = renderToStaticMarkup(<JobReceipt initial={job} live={false} />);

    expect(html).toContain('Merged');
    expect(html).toContain('Delivery commitment');
    expect(html).toContain('#7');
    expect(html).toContain('b'.repeat(40));
    expect(html).toContain('c'.repeat(64));
    expect(html).toContain('Merge and refund-liability evidence');
    expect(html).toContain('Discharged');
    expect(html).toContain('d'.repeat(64));
    expect(html).toContain('required exact-head approval');
  });

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
        reason:
          'The separate AI review approved the patch against the issue scope and repository checks.',
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
          reason:
            'The separate AI review did not approve the patch against the issue scope and repository checks.',
        },
        {
          phase: 'repair',
          status: 'failed',
          artifactHash: 'd'.repeat(64),
          reviewedAt: '2026-08-23T10:00:00.000Z',
          costUsd: 0.12,
          reason: 'The separate AI review could not be completed.',
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

    expect(html).toContain('Separate AI review record');
    expect(html).toContain('review-model');
    expect(html).toContain('provider-7');
    expect(html).toContain('request-9');
    expect(html).toContain('$0.175');
    expect(html).toContain('records provider work, not a successful delivery');
    expect(html).toContain('b'.repeat(64));
    expect(html).toContain('AI review history');
    expect(html).toContain('Not approved');
    expect(html).toContain('Review could not complete');
    expect(html).toContain(
      'The separate AI review did not approve the patch against the issue scope and repository checks.',
    );
    expect(html).toContain('The separate AI review could not be completed.');
    expect(html).toContain('c'.repeat(64));
    expect(html).toContain('d'.repeat(64));
    expect(html).not.toContain('4000000');
    expect(html).not.toContain('balanceRemaining');
  });
});
