import type { Job } from './types.js';

export interface RefundFollowup {
  createBounty(job: Job): Promise<unknown>;
  recordFailure(job: Job): Promise<unknown>;
  reportCapabilityFailure(job: Job, cause: unknown): void;
}

export async function runRefundFollowup(job: Job, followup: RefundFollowup): Promise<void> {
  const [bounty, capability] = await Promise.allSettled([
    followup.createBounty(job),
    followup.recordFailure(job),
  ]);

  if (capability.status === 'rejected') {
    try {
      followup.reportCapabilityFailure(job, capability.reason);
    } catch (cause) {
      console.error(
        `capability failure reporting failed for job ${job.id}: ${cause instanceof Error ? cause.message : String(cause)}`,
      );
    }
  }

  if (bounty.status === 'rejected') throw bounty.reason;
}
