import { parsePullRequestUrl } from './github.js';
import type { RefundLiability } from './policy-client.js';
import type { MizukiStore } from './store.js';
import type { Job } from './types.js';

type LiabilityDischarger = {
  dischargeRefundLiability(
    liabilityId: string,
    evidence: {
      jobId: string;
      settlementSignature: string;
      repository: string;
      pullRequestNumber: number;
    },
  ): Promise<RefundLiability>;
};

export async function finalizeJobMerge(
  store: MizukiStore,
  policy: LiabilityDischarger,
  paymentMode: 'mock' | 'live',
  job: Job,
  mergedAt: string,
): Promise<Job> {
  if (job.state !== 'delivered' || !job.prUrl) {
    throw new Error('only a delivered pull request can finalize a job merge');
  }
  if (job.mergedAt && job.refundLiabilityDischargedAt) return job;
  if (paymentMode === 'mock') {
    return job.mergedAt ? job : store.patchJob(job.id, { mergedAt });
  }
  if (!job.refundLiabilityId) {
    throw new Error('delivered job has no registered refund liability');
  }

  const pull = parsePullRequestUrl(job.prUrl);
  const liability = await policy.dischargeRefundLiability(job.refundLiabilityId, {
    jobId: job.id,
    settlementSignature: job.payment.transaction,
    repository: `${job.quote.owner}/${job.quote.repo}`,
    pullRequestNumber: pull.number,
  });
  if (
    liability.id !== job.refundLiabilityId ||
    liability.jobId !== job.id ||
    liability.settlementSignature !== job.payment.transaction ||
    !liability.dischargedAt ||
    !liability.dischargeEvidenceHash
  ) {
    throw new Error('refund liability discharge evidence does not match the job');
  }

  return store.patchJob(job.id, {
    mergedAt: job.mergedAt ?? mergedAt,
    refundLiabilityDischargedAt: liability.dischargedAt,
    refundLiabilityDischargeEvidenceHash: liability.dischargeEvidenceHash,
  });
}
