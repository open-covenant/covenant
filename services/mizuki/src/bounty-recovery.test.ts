import { describe, expect, it, vi } from 'vitest';
import { runBountyRecovery, type BountyRecoveryActions } from './bounty-recovery.js';
import type { Job } from './types.js';

describe('bounty recovery', () => {
  it('isolates failed jobs and phases so later recovery still runs', async () => {
    const first = { id: 'first', state: 'refunded' } as Job;
    const second = { id: 'second', state: 'refunded' } as Job;
    const recovered: string[] = [];
    const phases: string[] = [];
    const failures: string[] = [];
    const actions: BountyRecoveryActions = {
      jobs: async () => [first, second],
      recoverRefunded: async (job) => {
        recovered.push(job.id);
        if (job.id === first.id) throw new Error('escrow unavailable');
      },
      refreshMerged: async () => {
        phases.push('merged');
        throw new Error('github unavailable');
      },
      expireOffers: async () => {
        phases.push('offers');
      },
      expireClaims: async () => {
        phases.push('claims');
      },
      fundAwaiting: async () => {
        phases.push('funding');
      },
      reconcileFinancial: async () => {
        phases.push('financial');
        return { failed: 2 };
      },
      reportFailure: (context) => failures.push(context),
      reportPendingFinancial: vi.fn(),
    };

    await expect(runBountyRecovery(actions)).resolves.toBeUndefined();

    expect(recovered).toEqual(['first', 'second']);
    expect(phases).toEqual(['merged', 'offers', 'claims', 'funding', 'financial']);
    expect(failures).toEqual(['refunded job first', 'merge refresh']);
    expect(actions.reportPendingFinancial).toHaveBeenCalledWith(2);
  });

  it('continues lifecycle recovery when the job scan fails', async () => {
    const phases: string[] = [];
    const actions: BountyRecoveryActions = {
      jobs: async () => {
        throw new Error('database unavailable');
      },
      recoverRefunded: vi.fn(),
      refreshMerged: async () => {
        phases.push('merged');
      },
      expireOffers: async () => {
        phases.push('offers');
      },
      expireClaims: async () => {
        phases.push('claims');
      },
      fundAwaiting: async () => {
        phases.push('funding');
      },
      reconcileFinancial: async () => {
        phases.push('financial');
        return { failed: 0 };
      },
      reportFailure: vi.fn(),
      reportPendingFinancial: vi.fn(),
    };

    await runBountyRecovery(actions);

    expect(actions.recoverRefunded).not.toHaveBeenCalled();
    expect(actions.reportFailure).toHaveBeenCalledWith('job scan', expect.any(Error));
    expect(phases).toEqual(['merged', 'offers', 'claims', 'funding', 'financial']);
  });
});
