import { describe, expect, it } from 'vitest';
import { bountyClaimable } from './public-api.js';
import type { RescueBounty } from './types.js';

const at = (iso: string) => new Date(iso);
const bounty = (over: Partial<RescueBounty> = {}) =>
  ({
    state: 'open',
    offerExpiresAt: '2026-09-10T00:00:00.000Z',
    activeClaim: undefined,
    ...over,
  }) as RescueBounty;

describe('bountyClaimable', () => {
  const now = at('2026-09-04T00:00:00.000Z');

  it('is claimable when open, unclaimed, inside its window, and claiming is on', () => {
    expect(bountyClaimable(bounty(), true, now)).toBe(true);
  });

  it('is not claimable once the offer window has passed', () => {
    expect(bountyClaimable(bounty({ offerExpiresAt: '2026-09-01T00:00:00.000Z' }), true, now)).toBe(
      false,
    );
  });

  it('is not claimable while claiming is switched off, however live the offer looks', () => {
    // The exact case a contributor hit: a funded offer inside its window that
    // nobody was permitted to claim.
    expect(bountyClaimable(bounty(), false, now)).toBe(false);
  });

  it('is not claimable in any settled state', () => {
    for (const state of ['expired', 'released', 'rejected', 'refunded'] as const) {
      expect(bountyClaimable(bounty({ state }), true, now)).toBe(false);
    }
  });

  it('is not claimable while someone else holds the claim', () => {
    expect(
      bountyClaimable(
        bounty({
          activeClaim: { claimantId: 'c', leaseExpiresAt: '2026-09-05T00:00:00.000Z' } as never,
        }),
        true,
        now,
      ),
    ).toBe(false);
  });
});
