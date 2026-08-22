import { describe, expect, it, vi } from 'vitest';
import type { RefundLiability } from './policy-client.js';
import { finalizeJobMerge } from './merges.js';
import { MemoryStore } from './store.js';
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

async function deliveredJob(store: MemoryStore) {
  const { job } = await store.createJob(
    quote,
    { payer: 'customer', transaction: 'settlement-signature', amountAtomic: '2000000' },
    'payment-1',
  );
  return store.transitionJob(job.id, 'settlement_pending', 'delivered', {
    prUrl: 'https://github.com/example/tool/pull/12',
    refundLiabilityId: 'liability-1',
  });
}

function discharged(): RefundLiability {
  return {
    id: 'liability-1',
    jobId: expect.any(String) as unknown as string,
    settlementSignature: 'settlement-signature',
    payer: 'customer',
    treasury: 'treasury',
    mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
    decimals: 6,
    rawAmount: '2000000',
    amountUsdCents: 200,
    status: 'discharged',
    registeredAt: '2026-08-22T10:00:00.000Z',
    dischargedAt: '2026-08-22T12:00:00.000Z',
    dischargeEvidenceHash: 'e'.repeat(64),
  };
}

describe('job merge finalization', () => {
  it('persists merge evidence only after the refund liability is discharged', async () => {
    const store = new MemoryStore();
    const job = await deliveredJob(store);
    const liability = discharged();
    liability.jobId = job.id;
    const policy = { dischargeRefundLiability: vi.fn(async () => liability) };

    const merged = await finalizeJobMerge(store, policy, 'live', job, '2026-08-22T12:00:00.000Z');

    expect(policy.dischargeRefundLiability).toHaveBeenCalledWith('liability-1', {
      jobId: job.id,
      settlementSignature: 'settlement-signature',
      repository: 'example/tool',
      pullRequestNumber: 12,
    });
    expect(merged).toMatchObject({
      mergedAt: '2026-08-22T12:00:00.000Z',
      refundLiabilityDischargedAt: '2026-08-22T12:00:00.000Z',
      refundLiabilityDischargeEvidenceHash: 'e'.repeat(64),
    });
  });

  it('recovers a merge recorded before its liability discharge', async () => {
    const store = new MemoryStore();
    const job = await deliveredJob(store);
    const recorded = await store.patchJob(job.id, { mergedAt: '2026-08-22T11:00:00.000Z' });
    const liability = discharged();
    liability.jobId = job.id;

    const recovered = await finalizeJobMerge(
      store,
      { dischargeRefundLiability: vi.fn(async () => liability) },
      'live',
      recorded,
      '2026-08-22T12:00:00.000Z',
    );

    expect(recovered.mergedAt).toBe('2026-08-22T11:00:00.000Z');
    expect(recovered.refundLiabilityDischargedAt).toBe('2026-08-22T12:00:00.000Z');
  });

  it('does not persist a merge when discharge evidence is incomplete', async () => {
    const store = new MemoryStore();
    const job = await deliveredJob(store);
    const liability = discharged();
    liability.jobId = job.id;
    delete liability.dischargeEvidenceHash;

    await expect(
      finalizeJobMerge(
        store,
        { dischargeRefundLiability: vi.fn(async () => liability) },
        'live',
        job,
        '2026-08-22T12:00:00.000Z',
      ),
    ).rejects.toThrow('discharge evidence does not match');
    expect((await store.job(job.id))?.mergedAt).toBeUndefined();
  });
});
