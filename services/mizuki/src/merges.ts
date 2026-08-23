import { parsePullRequestUrl } from './github.js';
import {
  refundLiabilityCommitment,
  type RefundLiability,
  type RefundLiabilityDischarge,
} from './policy-client.js';
import type { MizukiStore } from './store.js';
import type { Job } from './types.js';

type LiabilityDischarger = {
  dischargeRefundLiability(
    liabilityId: string,
    evidence: RefundLiabilityDischarge,
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
  if (!job.deliveryCommitSha || !job.deliveryEvidence) {
    throw new Error('delivered job has no immutable reviewed delivery evidence');
  }

  const pull = parsePullRequestUrl(job.prUrl);
  if (pull.number !== job.deliveryEvidence.pullRequestNumber) {
    throw new Error('delivered pull request does not match the reviewed delivery evidence');
  }
  const liability = await policy.dischargeRefundLiability(job.refundLiabilityId, {
    jobId: job.id,
    settlementSignature: job.payment.transaction,
    repository: `${job.quote.owner}/${job.quote.repo}`,
    issueNumber: job.quote.issueNumber,
    pullRequestNumber: pull.number,
    deliveredCommitSha: job.deliveryCommitSha,
    reviewedHeadSha: job.deliveryEvidence.headSha,
    reviewedBaseSha: job.deliveryEvidence.baseSha,
    reviewedBaseRef: job.deliveryEvidence.baseRef,
    reviewedDiffHash: job.deliveryEvidence.diffHash,
  });
  const commitment = refundLiabilityCommitment(job.quote);
  if (
    liability.id !== job.refundLiabilityId ||
    liability.jobId !== job.id ||
    liability.settlementSignature !== job.payment.transaction ||
    liability.repository !== commitment.repository ||
    liability.issueNumber !== commitment.issueNumber ||
    liability.baseRef !== commitment.baseRef ||
    liability.baseSha !== commitment.baseSha ||
    liability.repositoryAuthorizedAt !== commitment.repositoryAuthorizedAt ||
    liability.authorizationEvidenceHash !== commitment.authorizationEvidenceHash ||
    liability.reviewedHeadSha !== job.deliveryEvidence.headSha ||
    liability.reviewedBaseSha !== job.deliveryEvidence.baseSha ||
    liability.reviewedBaseRef !== job.deliveryEvidence.baseRef ||
    liability.reviewedDiffHash !== job.deliveryEvidence.diffHash ||
    !liability.deliveryBoundAt ||
    !liability.deliveryBindingHash ||
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
