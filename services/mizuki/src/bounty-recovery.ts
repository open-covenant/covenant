import type { Job } from './types.js';

export interface BountyRecoveryActions {
  jobs(): Promise<Job[]>;
  recoverRefunded(job: Job): Promise<void>;
  refreshMerged(): Promise<void>;
  expireOffers(): Promise<unknown>;
  expireClaims(): Promise<unknown>;
  fundAwaiting(): Promise<unknown>;
  reconcileFinancial(): Promise<{ failed: number }>;
  reportFailure(context: string, cause: unknown): void;
  reportPendingFinancial(count: number): void;
}

export async function runBountyRecovery(actions: BountyRecoveryActions): Promise<void> {
  let jobs: Job[] = [];
  try {
    jobs = await actions.jobs();
  } catch (cause) {
    actions.reportFailure('job scan', cause);
  }

  for (const job of jobs) {
    if (job.state !== 'refunded') continue;
    try {
      await actions.recoverRefunded(job);
    } catch (cause) {
      actions.reportFailure(`refunded job ${job.id}`, cause);
    }
  }

  await attempt(actions, 'merge refresh', actions.refreshMerged);
  await attempt(actions, 'offer expiry', actions.expireOffers);
  await attempt(actions, 'claim expiry', actions.expireClaims);
  await attempt(actions, 'escrow funding', actions.fundAwaiting);
  const recovery = await attempt(actions, 'financial reconciliation', actions.reconcileFinancial);
  if (recovery && recovery.failed > 0) actions.reportPendingFinancial(recovery.failed);
}

async function attempt<T>(
  actions: BountyRecoveryActions,
  context: string,
  operation: () => Promise<T>,
): Promise<T | undefined> {
  try {
    return await operation();
  } catch (cause) {
    actions.reportFailure(context, cause);
    return undefined;
  }
}
