import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { Job, ProviderRouteReceipt } from '@/lib/types';
import { JobReceipt, jobPollingComplete, shouldApplyJobUpdate } from './job-receipt';

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

  it.each(['failed', 'rejected'] as const)(
    'keeps live status updates active while a %s job enters refund recovery',
    (state) => {
      const html = renderToStaticMarkup(
        <JobReceipt
          initial={{
            id: `job-${state}`,
            state,
            issueUrl: 'https://github.com/public/tool/issues/2',
            class: 'micro',
            priceAtomic: '2000000',
            error: 'Delivery stopped before refund recovery completed.',
            changedFiles: [],
            validations: [],
            variableRouteCostEstimateUsd: 0.12,
            costCoverage: {
              included: [
                'gateway_model_token_rate_estimate',
                'gateway_sandbox_runtime_estimate',
                'reviewer_model_token_rate_estimate',
              ],
              excluded: [
                'provider_billing_adjustments',
                'chain_and_facilitator_fees',
                'infrastructure',
              ],
            },
            createdAt: '2026-08-25T08:00:00.000Z',
            updatedAt: '2026-08-25T08:01:00.000Z',
          }}
        />,
      );

      expect(html).toContain('Live');
      expect(html).toContain('Full refund in progress');
    },
  );

  it('shows provider work on a refunded job without exposing the funded balance', () => {
    const provider = {
      model: 'review-model',
      resolvedModel: 'review-model-20260825',
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
    expect(html).toContain('Requested model');
    expect(html).toContain('review-model');
    expect(html).toContain('Returned model');
    expect(html).toContain('review-model-20260825');
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

  it('keeps a delivered job live until merge and refund-liability discharge are recorded', () => {
    const delivered = jobFixture({ state: 'delivered' });
    const merged = jobFixture({ state: 'delivered', mergedAt: '2026-08-25T09:02:00.000Z' });
    const discharged = jobFixture({
      state: 'delivered',
      mergedAt: '2026-08-25T09:02:00.000Z',
      refundLiabilityDischarge: {
        dischargedAt: '2026-08-25T09:02:03.000Z',
        evidenceHash: 'e'.repeat(64),
      },
    });

    expect(jobPollingComplete(delivered)).toBe(false);
    expect(jobPollingComplete(merged)).toBe(false);
    expect(jobPollingComplete(discharged)).toBe(true);
    expect(renderToStaticMarkup(<JobReceipt initial={merged} />)).toContain('Live');
    expect(renderToStaticMarkup(<JobReceipt initial={discharged} />)).not.toContain('Live');
  });

  it('shows an expired authorization as unpaid and terminal', () => {
    const expired = jobFixture({
      state: 'payment_expired',
      paymentTransaction: undefined,
      error: 'Payment authorization expired without settlement',
    });
    const html = renderToStaticMarkup(<JobReceipt initial={expired} />);

    expect(jobPollingComplete(expired)).toBe(true);
    expect(html).toContain('No payment settled');
    expect(html).toContain('No refund is required');
    expect(html).not.toContain('Full refund in progress');
    expect(html).not.toContain('Live');
  });

  it('accepts only newer status records for the same unfinished job', () => {
    const current = jobFixture({ updatedAt: '2026-08-25T09:01:00.000Z' });

    expect(
      shouldApplyJobUpdate(current, jobFixture({ updatedAt: '2026-08-25T09:01:01.000Z' })),
    ).toBe(true);
    expect(
      shouldApplyJobUpdate(current, jobFixture({ updatedAt: '2026-08-25T09:01:00.000Z' })),
    ).toBe(false);
    expect(
      shouldApplyJobUpdate(current, jobFixture({ updatedAt: '2026-08-25T09:00:59.000Z' })),
    ).toBe(false);
    expect(
      shouldApplyJobUpdate(
        current,
        jobFixture({ id: 'job-other', updatedAt: '2026-08-25T09:01:01.000Z' }),
      ),
    ).toBe(false);
  });
});

function jobFixture(patch: Partial<Job> = {}): Job {
  return {
    id: 'job-live',
    state: 'running',
    issueUrl: 'https://github.com/public/tool/issues/3',
    class: 'micro',
    priceAtomic: '2000000',
    changedFiles: [],
    validations: [],
    variableRouteCostEstimateUsd: 0.12,
    costCoverage: {
      included: [
        'gateway_model_token_rate_estimate',
        'gateway_sandbox_runtime_estimate',
        'reviewer_model_token_rate_estimate',
      ],
      excluded: ['provider_billing_adjustments', 'chain_and_facilitator_fees', 'infrastructure'],
    },
    createdAt: '2026-08-25T09:00:00.000Z',
    updatedAt: '2026-08-25T09:01:00.000Z',
    ...patch,
  };
}
