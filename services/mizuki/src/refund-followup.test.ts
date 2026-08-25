import { describe, expect, it, vi } from 'vitest';
import { runRefundFollowup } from './refund-followup.js';
import type { Job } from './types.js';

describe('runRefundFollowup', () => {
  it('keeps a recorded bounty successful when capability recording fails', async () => {
    const createBounty = vi.fn(async () => {});
    const recordFailure = vi.fn(async () => {
      throw new Error('capability persistence unavailable');
    });
    const reportCapabilityFailure = vi.fn();

    await expect(
      runRefundFollowup(job(), { createBounty, recordFailure, reportCapabilityFailure }),
    ).resolves.toBeUndefined();
    expect(createBounty).toHaveBeenCalledOnce();
    expect(recordFailure).toHaveBeenCalledOnce();
    expect(reportCapabilityFailure).toHaveBeenCalledOnce();
  });

  it('still records the failure before surfacing a bounty creation error', async () => {
    const bountyError = new Error('bounty funding unavailable');
    const createBounty = vi.fn(async () => {
      throw bountyError;
    });
    const recordFailure = vi.fn(async () => {});
    const reportCapabilityFailure = vi.fn();

    await expect(
      runRefundFollowup(job(), { createBounty, recordFailure, reportCapabilityFailure }),
    ).rejects.toBe(bountyError);
    expect(recordFailure).toHaveBeenCalledOnce();
    expect(reportCapabilityFailure).not.toHaveBeenCalled();
  });
});

function job(): Job {
  return {
    id: '11111111-1111-4111-8111-111111111111',
    idempotencyKey: 'refund-followup',
    quote: {
      id: '22222222-2222-4222-8222-222222222222',
      issueUrl: 'https://github.com/example/project/issues/1',
      owner: 'example',
      repo: 'project',
      issueNumber: 1,
      issueTitle: 'Fix issue',
      issueBody: '',
      baseSha: 'a'.repeat(40),
      defaultBranch: 'main',
      class: 'micro',
      priceAtomic: '2000000',
      maxFiles: 3,
      maxCostUsd: 0.8,
      validationCommands: [],
      expiresAt: '2099-01-01T00:00:00Z',
    },
    payment: { payer: '1'.repeat(32), transaction: 'payment', amountAtomic: '2000000' },
    state: 'refunded',
    createdAt: '2026-08-22T00:00:00.000Z',
    updatedAt: '2026-08-22T00:00:00.000Z',
    inputTokens: 0,
    outputTokens: 0,
    estimatedCostUsd: 0,
    version: 1,
  };
}
