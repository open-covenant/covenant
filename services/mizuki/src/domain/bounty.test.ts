import { describe, expect, it } from 'vitest';
import {
  beginRescueBountyDisputeResolution,
  calculateRescueBountyPriceCents,
  claimRescueBounty,
  createRescueBounty,
  expireRescueBountyClaim,
  finalizeRescueBountyDisputeResolution,
  finalizeRescueBountyClaimRefund,
  handoffRescueBountyDisputeReleaseToRefund,
  openRescueBountyDispute,
  submitDraftPullRequest,
  transitionRescueBounty,
  type RescueBounty,
} from './bounty.js';
import { DomainRuleError } from './state-machine.js';

const T0 = '2026-08-22T10:00:00.000Z';

function draft(): RescueBounty {
  return createRescueBounty({
    id: 'bounty-1',
    sourceJobId: 'job-1',
    failureReceiptId: 'failure-1',
    repository: 'Example/Project',
    issueNumber: 12,
    issueUrl: 'https://github.com/example/project/issues/12',
    jobPriceCents: 200,
    at: T0,
  });
}

function open(): RescueBounty {
  const funding = transitionRescueBounty(draft(), 'funding', {
    at: T0,
    expectedRevision: 0,
  });
  return transitionRescueBounty(funding, 'open', {
    at: '2026-08-22T10:01:00.000Z',
    expectedRevision: 1,
  });
}

function claimed(): RescueBounty {
  return claimRescueBounty(open(), {
    claimId: 'claim-1',
    claimantId: 'github:42',
    walletAddress: 'wallet-1',
    at: '2026-08-22T11:00:00.000Z',
    expectedRevision: 2,
  });
}

describe('rescue bounty pricing', () => {
  it('applies the minimum, double-price rule, and cap', () => {
    expect(calculateRescueBountyPriceCents(200)).toBe(1_000);
    expect(calculateRescueBountyPriceCents(1_000)).toBe(2_000);
    expect(calculateRescueBountyPriceCents(2_000)).toBe(2_500);
  });

  it('rejects zero, negative, and unsafe prices', () => {
    expect(() => calculateRescueBountyPriceCents(0)).toThrow();
    expect(() => calculateRescueBountyPriceCents(-1)).toThrow();
    expect(() => calculateRescueBountyPriceCents(Number.MAX_SAFE_INTEGER)).toThrow();
  });
});

describe('rescue bounty lifecycle', () => {
  it('creates a normalized draft linked to the authorized issue', () => {
    expect(draft()).toMatchObject({
      repository: 'example/project',
      issueNumber: 12,
      priceCents: 1_000,
      state: 'draft',
      revision: 0,
    });
    expect(() =>
      createRescueBounty({
        ...draft(),
        jobPriceCents: 200,
        at: T0,
        issueUrl: 'https://github.com/example/other/issues/12',
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_ISSUE_URL',
      }),
    );
  });

  it('enforces transition order and optimistic revisions', () => {
    expect(() =>
      transitionRescueBounty(draft(), 'open', {
        at: T0,
        expectedRevision: 0,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_TRANSITION',
      }),
    );
    expect(() =>
      transitionRescueBounty(open(), 'expired', {
        at: '2026-08-23T10:00:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'STALE_REVISION',
      }),
    );
  });

  it('can refund an unfunded or unclaimed bounty without inventing a claimant', () => {
    const funding = transitionRescueBounty(draft(), 'funding', {
      at: T0,
      expectedRevision: 0,
    });
    const refunded = transitionRescueBounty(funding, 'refunded', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 1,
    });
    expect(refunded).toMatchObject({ state: 'refunded', revision: 2 });
    expect(refunded.activeClaim).toBeUndefined();
  });

  it('grants exactly one active 48-hour claim lease', () => {
    const bounty = claimed();
    expect(bounty).toMatchObject({
      state: 'claimed',
      revision: 3,
      activeClaim: {
        state: 'active',
        leaseExpiresAt: '2026-08-24T11:00:00.000Z',
      },
    });
    expect(() =>
      claimRescueBounty(bounty, {
        claimId: 'claim-2',
        claimantId: 'github:99',
        walletAddress: 'wallet-2',
        at: '2026-08-22T11:01:00.000Z',
        expectedRevision: 3,
      }),
    ).toThrow();
  });

  it('uses revisions to make competing claim commands mutually exclusive at persistence time', () => {
    const bounty = open();
    const winner = claimRescueBounty(bounty, {
      claimId: 'claim-1',
      claimantId: 'github:42',
      walletAddress: 'wallet-1',
      at: '2026-08-22T11:00:00.000Z',
      expectedRevision: 2,
    });
    expect(() =>
      claimRescueBounty(winner, {
        claimId: 'claim-2',
        claimantId: 'github:99',
        walletAddress: 'wallet-2',
        at: '2026-08-22T11:00:00.000Z',
        expectedRevision: 2,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'STALE_REVISION',
      }),
    );
  });

  it('keeps the signer-backed deadline unchanged for a repository draft PR', () => {
    const bounty = submitDraftPullRequest(claimed(), {
      pullRequestUrl: 'https://github.com/example/project/pull/44',
      at: '2026-08-22T12:00:00.000Z',
      expectedRevision: 3,
    });
    expect(bounty).toMatchObject({
      state: 'pr_submitted',
      revision: 4,
      activeClaim: {
        leaseExpiresAt: '2026-08-24T11:00:00.000Z',
        draftPullRequestUrl: 'https://github.com/example/project/pull/44',
      },
    });
    expect(() =>
      submitDraftPullRequest(bounty, {
        pullRequestUrl: 'https://github.com/example/project/pull/45',
        at: '2026-08-22T13:00:00.000Z',
        expectedRevision: 4,
      }),
    ).toThrow();
  });

  it('rejects late drafts and PRs from other repositories', () => {
    expect(() =>
      submitDraftPullRequest(claimed(), {
        pullRequestUrl: 'https://github.com/example/project/pull/44',
        at: '2026-08-24T11:00:00.000Z',
        expectedRevision: 3,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'CLAIM_EXPIRED',
      }),
    );
    expect(() =>
      submitDraftPullRequest(claimed(), {
        pullRequestUrl: 'https://github.com/example/other/pull/44',
        at: '2026-08-22T12:00:00.000Z',
        expectedRevision: 3,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_PULL_REQUEST_URL',
      }),
    );
  });

  it('reopens an expired claim and preserves its history', () => {
    const bounty = claimed();
    expect(() =>
      expireRescueBountyClaim(bounty, {
        at: '2026-08-24T10:59:59.999Z',
        expectedRevision: 3,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'CLAIM_STILL_ACTIVE',
      }),
    );

    const pending = expireRescueBountyClaim(bounty, {
      at: '2026-08-24T11:00:00.000Z',
      expectedRevision: 3,
    });
    expect(pending.state).toBe('claim_refund_pending');
    expect(pending.activeClaim).toMatchObject({ id: 'claim-1', state: 'expired' });
    expect(pending.claimHistory).toEqual([]);

    const submitted = submitDraftPullRequest(claimed(), {
      pullRequestUrl: 'https://github.com/example/project/pull/45',
      at: '2026-08-22T12:00:00.000Z',
      expectedRevision: 3,
    });
    const validating = transitionRescueBounty(submitted, 'validating', {
      at: '2026-08-22T13:00:00.000Z',
      expectedRevision: 4,
    });
    expect(
      expireRescueBountyClaim(validating, {
        at: '2026-08-24T11:00:00.000Z',
        expectedRevision: 5,
      }),
    ).toMatchObject({ state: 'claim_refund_pending', activeClaim: { state: 'expired' } });

    const refunded = finalizeRescueBountyClaimRefund(pending, {
      at: '2026-08-24T11:01:00.000Z',
      expectedRevision: 4,
    });
    expect(refunded.state).toBe('refunded');
    expect(refunded.activeClaim).toBeUndefined();
    expect(refunded.claimHistory).toEqual([
      expect.objectContaining({ id: 'claim-1', state: 'expired' }),
    ]);
  });

  it('advances a submitted contribution through validation and release', () => {
    let bounty = submitDraftPullRequest(claimed(), {
      pullRequestUrl: 'https://github.com/example/project/pull/44',
      at: '2026-08-22T12:00:00.000Z',
      expectedRevision: 3,
    });
    bounty = transitionRescueBounty(bounty, 'validating', {
      at: '2026-08-22T13:00:00.000Z',
      expectedRevision: 4,
    });
    bounty = transitionRescueBounty(bounty, 'accepted', {
      at: '2026-08-22T14:00:00.000Z',
      expectedRevision: 5,
    });
    bounty = transitionRescueBounty(bounty, 'released', {
      at: '2026-08-22T15:00:00.000Z',
      expectedRevision: 6,
    });
    expect(bounty).toMatchObject({
      state: 'released',
      revision: 7,
      activeClaim: { state: 'released', closedAt: '2026-08-22T15:00:00.000Z' },
    });
    expect(() =>
      transitionRescueBounty(bounty, 'refunded', {
        at: '2026-08-22T16:00:00.000Z',
        expectedRevision: 7,
      }),
    ).toThrow();
  });

  it('records an evidence-backed dispute decision and final transaction', () => {
    const submitted = submitDraftPullRequest(claimed(), {
      pullRequestUrl: 'https://github.com/example/project/pull/44',
      at: '2026-08-22T12:00:00.000Z',
      expectedRevision: 3,
    });
    const disputed = openRescueBountyDispute(submitted, {
      disputeId: 'dispute-1',
      claimantId: 'github:42',
      reason: 'The independent review missed the attached passing checks.',
      at: '2026-08-22T13:00:00.000Z',
      expectedRevision: 4,
    });
    const pending = beginRescueBountyDisputeResolution(disputed, {
      disputeId: 'dispute-1',
      resolutionId: 'resolution-1',
      idempotencyKey: 'resolve:dispute-1',
      decision: 'release',
      evidence: {
        summary: 'The merged patch and CI results satisfy the bounty acceptance criteria.',
        references: ['https://github.com/example/project/pull/44'],
      },
      at: '2026-08-22T14:00:00.000Z',
      expectedRevision: 5,
    });
    const released = finalizeRescueBountyDisputeResolution(pending, {
      decision: 'release',
      transactionSignature: 'release-tx',
      at: '2026-08-22T15:00:00.000Z',
      expectedRevision: 6,
    });

    expect(released).toMatchObject({
      state: 'released',
      activeClaim: { state: 'released' },
      dispute: {
        id: 'dispute-1',
        state: 'released',
        resolution: {
          requestedDecision: 'release',
          settlementDecision: 'release',
          transactionSignature: 'release-tx',
        },
      },
    });
  });

  it('allows a failed release decision to hand off to refund only at the immutable deadline', () => {
    const submitted = submitDraftPullRequest(claimed(), {
      pullRequestUrl: 'https://github.com/example/project/pull/44',
      at: '2026-08-22T12:00:00.000Z',
      expectedRevision: 3,
    });
    const disputed = openRescueBountyDispute(submitted, {
      disputeId: 'dispute-1',
      claimantId: 'github:42',
      reason: 'The release should be reviewed against the merge receipt.',
      at: '2026-08-22T13:00:00.000Z',
      expectedRevision: 4,
    });
    const pending = beginRescueBountyDisputeResolution(disputed, {
      disputeId: 'dispute-1',
      resolutionId: 'resolution-1',
      idempotencyKey: 'resolve:dispute-1',
      decision: 'release',
      evidence: {
        summary: 'The repository evidence supports release if the on-chain deadline permits it.',
        references: ['https://github.com/example/project/pull/44'],
      },
      at: '2026-08-22T14:00:00.000Z',
      expectedRevision: 5,
    });
    expect(() =>
      handoffRescueBountyDisputeReleaseToRefund(pending, {
        at: '2026-08-24T10:59:59.999Z',
        expectedRevision: 6,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'CLAIM_STILL_ACTIVE',
      }),
    );

    const refunding = handoffRescueBountyDisputeReleaseToRefund(pending, {
      at: '2026-08-24T11:00:00.000Z',
      expectedRevision: 6,
    });
    expect(refunding.dispute?.resolution).toMatchObject({
      requestedDecision: 'release',
      settlementDecision: 'refund',
      fallbackReason: 'release_deadline_elapsed',
    });
    expect(() =>
      beginRescueBountyDisputeResolution(disputed, {
        disputeId: 'dispute-1',
        resolutionId: 'resolution-2',
        idempotencyKey: 'resolve-without-evidence',
        decision: 'refund',
        evidence: { summary: 'Too short', references: [] },
        at: '2026-08-22T14:00:00.000Z',
        expectedRevision: 5,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_DISPUTE_EVIDENCE',
      }),
    );
  });
});
