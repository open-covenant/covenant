import { describe, expect, it } from 'vitest';
import { BountyService, type ContributorPatchReviewer } from './bounties.js';
import { fingerprintBountyDisputeEvidence, type ContributorEscrow } from './domain/index.js';
import { PolicyRequestError, type FinancialPolicy, type PolicyOperation } from './policy-client.js';
import { MemoryStore } from './store.js';
import type { Job, Quote } from './types.js';

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl: 'https://github.com/example/project/issues/1',
  owner: 'example',
  repo: 'project',
  issueNumber: 1,
  issueTitle: 'Fix parser edge case',
  issueBody: 'Handle empty input.',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  installationId: 1,
  class: 'standard',
  priceAtomic: '10000000',
  maxFiles: 10,
  maxCostUsd: 4,
  validationCommands: [],
  expiresAt: '2099-01-01T00:00:00Z',
};
const reviewedHeadSha = 'b'.repeat(40);
const reviewedBaseSha = 'a'.repeat(40);
const reviewedDiffHash = 'c'.repeat(64);
const bountyConfig = { escrowRefundTo: 'treasury' };

describe('BountyService', () => {
  it('uses finalized SOL escrow as the sole funding gate without a manual USD ledger seed', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    const service = new BountyService(
      store,
      new MockPolicy(),
      reviewer({ approved: true, reason: 'scoped and correct' }),
      tickingClock(),
      bountyConfig,
    );

    const bounty = await service.createAfterRefund(job);

    expect(bounty.state).toBe('open');
    expect(await store.escrowByBounty(bounty.id)).toMatchObject({
      state: 'funded',
      amountAtomic: '2000000000',
      fundingSignature: expect.any(String),
    });
    expect(await store.ledgerEntries()).toEqual([
      expect.objectContaining({
        kind: 'bounty_reserved',
        asset: 'SOL',
        amountAtomic: '2000000000',
        transaction: expect.any(String),
      }),
    ]);
  });

  it('releases payment when the merged head and diff exactly match the independent review', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-1',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
      transaction: 'deposit-tx',
    });
    const policy = new MockPolicy();
    const service = new BountyService(
      store,
      policy,
      reviewer({ approved: true, reason: 'scoped and correct' }),
      tickingClock(),
      bountyConfig,
    );
    const created = await service.createAfterRefund(job);
    expect(created).toMatchObject({ state: 'open', priceCents: 2000 });

    const contributor = await store.upsertContributor('42', 'maintainer');
    const challenge = await service.createClaimChallenge(
      created.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    const claimed = await service.claim(created.id, contributor, challenge.id, 'signature');
    expect(claimed.state).toBe('claimed');
    expect((await store.escrowByBounty(created.id))?.state).toBe('bound');

    const reviewed = await service.submitPullRequest(
      created.id,
      contributor,
      'https://github.com/example/project/pull/2',
    );
    expect(reviewed.validationReceipt?.approved).toBe(true);
    expect(reviewed.validationReceipt).toMatchObject({
      headSha: reviewedHeadSha,
      baseSha: reviewedBaseSha,
      baseRef: 'main',
      diffHash: reviewedDiffHash,
    });
    const released = await service.releaseMerged(
      created.id,
      'https://github.com/example/project/pull/2',
    );
    expect(released.state).toBe('released');
    expect((await store.escrowByBounty(created.id))?.state).toBe('released');
    expect(policy.releaseInputs).toEqual([
      {
        pullRequestNumber: 2,
        reviewedHeadSha,
        reviewedDiffHash,
      },
    ]);
  });

  it('does not release when commits are pushed after independent approval', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    const policy = new MockPolicy();
    const service = new BountyService(
      store,
      policy,
      reviewer(
        { approved: true, reason: 'revision A is correct' },
        { headSha: 'd'.repeat(40), diffHash: 'e'.repeat(64) },
      ),
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('stale-review', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/12';
    const reviewed = await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);

    expect(reviewed.validationReceipt).toMatchObject({
      approved: true,
      headSha: reviewedHeadSha,
      baseSha: reviewedBaseSha,
      baseRef: 'main',
      diffHash: reviewedDiffHash,
    });
    await expect(service.releaseMerged(bounty.id, pullRequestUrl)).rejects.toThrow(
      'does not match the independently reviewed revision',
    );
    expect(policy.releaseInputs).toEqual([]);
    expect(await store.bounty(bounty.id)).toMatchObject({ state: 'pr_submitted' });
    expect(await store.escrowByBounty(bounty.id)).toMatchObject({ state: 'bound' });
  });

  it('allows only one concurrent claimant', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-2',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    const service = new BountyService(
      store,
      new MockPolicy(),
      reviewer(),
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    for (const [id, login, wallet] of [
      ['1', 'one', '1'.repeat(32)],
      ['2', 'two', '2'.repeat(32)],
    ]) {
      await store.upsertContributor(id, login);
      void wallet;
    }
    const one = (await store.contributor('1'))!;
    const two = (await store.contributor('2'))!;
    const oneChallenge = await service.createClaimChallenge(
      bounty.id,
      one,
      '1'.repeat(32),
      randomGrantId(),
    );
    const twoChallenge = await service.createClaimChallenge(
      bounty.id,
      two,
      '2'.repeat(32),
      randomGrantId(),
    );
    const results = await Promise.allSettled([
      service.claim(bounty.id, one, oneChallenge.id, 'signature-one'),
      service.claim(bounty.id, two, twoChallenge.id, 'signature-two'),
    ]);
    expect(results.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect((await store.bounty(bounty.id))?.state).toBe('claimed');
  });

  it('keeps an expired claim locked until its refund finalizes, then funds a new generation', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-expiry',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    let nowMs = Date.now() + 1_000;
    const now = () => new Date(nowMs);
    const policy = new MockPolicy(now);
    const service = new BountyService(store, policy, reviewer(), now, bountyConfig);
    const first = await service.createAfterRefund(job);
    const claimant = await store.upsertContributor('claimant', 'claimant');
    const challenge = await service.createClaimChallenge(
      first.id,
      claimant,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(first.id, claimant, challenge.id, 'signature');

    nowMs = Date.parse(challenge.claimExpiresAt);
    policy.failEscrowRefund = true;
    expect(await service.expireClaims()).toBe(1);
    expect((await store.bounty(first.id))?.state).toBe('claim_refund_pending');
    const second = await store.upsertContributor('second', 'second');
    await expect(
      service.createClaimChallenge(first.id, second, '2'.repeat(32), randomGrantId()),
    ).rejects.toThrow('not accepting claims');

    policy.failEscrowRefund = false;
    expect(await service.reconcileFinancialOperations()).toMatchObject({ failed: 0 });
    expect((await store.bounty(first.id))?.state).toBe('refunded');
    expect((await store.escrowByBounty(first.id))?.state).toBe('refunded');
    const replacement = await store.bountyBySourceJob(job.id);
    expect(replacement).toMatchObject({
      generation: 1,
      predecessorBountyId: first.id,
      state: 'open',
    });
    expect((await store.escrowByBounty(replacement!.id))?.state).toBe('funded');
  });

  it('closes an unclaimed offer only after its escrow refund finalizes', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-offer-expiry',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    let nowMs = Date.parse('2026-08-22T10:00:00.000Z');
    const now = () => new Date(nowMs);
    const policy = new MockPolicy(now);
    policy.escrowRefundRecipient = 'escrow-authority';
    const service = new BountyService(store, policy, reviewer(), now, {
      escrowRefundTo: 'escrow-authority',
    });
    const bounty = await service.createAfterRefund(job);

    nowMs = Date.parse(bounty.offerExpiresAt);
    policy.failEscrowRefund = true;
    expect(await service.expireOffers()).toBe(1);
    expect((await store.bounty(bounty.id))?.state).toBe('offer_refund_pending');

    policy.failEscrowRefund = false;
    expect(await service.reconcileFinancialOperations()).toMatchObject({ recovered: 1, failed: 0 });
    expect((await store.bounty(bounty.id))?.state).toBe('expired');
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('refunded');
    expect(await store.bountyBySourceJob(job.id)).toMatchObject({ id: bounty.id, generation: 0 });
  });

  it('refunds a merged bounty when release becomes impossible at the immutable deadline', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-expired-release',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    let nowMs = Date.now() + 1_000;
    const now = () => new Date(nowMs);
    const policy = new MockPolicy(now);
    const service = new BountyService(store, policy, reviewer(), now, bountyConfig);
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('late', 'late');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/8';
    await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);

    nowMs = Date.parse(challenge.claimExpiresAt);
    policy.releaseFailuresRemaining = 10;
    const closed = await service.releaseMerged(bounty.id, pullRequestUrl);
    expect(closed.state).toBe('refunded');
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('refunded');
    expect(await store.bountyBySourceJob(job.id)).toMatchObject({ id: bounty.id, generation: 0 });
  });

  it('records a release that won the deadline race instead of attempting a refund', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-release-race',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    let nowMs = Date.now() + 1_000;
    const now = () => new Date(nowMs);
    const policy = new MockPolicy(now);
    const service = new BountyService(store, policy, reviewer(), now, bountyConfig);
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('race', 'race');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/9';
    await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);

    nowMs = Date.parse(challenge.claimExpiresAt);
    policy.releaseFailuresRemaining = 1;
    const released = await service.releaseMerged(bounty.id, pullRequestUrl);
    expect(released.state).toBe('released');
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('released');
  });

  it('recovers a finalized bind after the local completion write fails', async () => {
    const store = new FailOnceBoundStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-bind-recovery',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    const service = new BountyService(
      store,
      new MockPolicy(),
      reviewer(),
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('recover', 'recover');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    store.failNextBound = true;
    await expect(service.claim(bounty.id, contributor, challenge.id, 'signature')).rejects.toThrow(
      'injected bound write failure',
    );
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('bind_pending');
    expect((await store.bounty(bounty.id))?.state).toBe('open');

    expect(await service.reconcileFinancialOperations()).toMatchObject({ failed: 0 });
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('bound');
    expect((await store.bounty(bounty.id))?.state).toBe('claimed');
  });

  it('checkpoints the escrow dispute and recovers an idempotent release after a local write failure', async () => {
    const store = new FailOnceResolutionStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-dispute-release',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    const policy = new MockPolicy();
    const service = new BountyService(store, policy, reviewer(), tickingClock(), bountyConfig);
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('dispute-release', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/10';
    await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);
    const reason = 'The reviewer ignored the linked successful repository checks.';
    const disputed = await service.openDispute(bounty.id, contributor, reason);
    const replay = await service.openDispute(bounty.id, contributor, reason);

    expect(replay.dispute?.id).toBe(disputed.dispute?.id);
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('disputed');
    expect(
      (await store.activity(500)).filter((event) => event.kind === 'bounty.disputed'),
    ).toHaveLength(1);

    const resolution = {
      decision: 'release' as const,
      evidence: {
        summary: '  The merged pull request and its checks meet the written acceptance criteria.  ',
        references: [`  ${pullRequestUrl}  `],
      },
      idempotencyKey: 'resolve:dispute-release',
    };
    store.failNextResolution = true;
    await expect(
      service.resolveDispute(bounty.id, disputed.dispute!.id, resolution),
    ).rejects.toThrow('injected resolution write failure');
    expect((await store.bounty(bounty.id))?.dispute?.state).toBe('release_pending');
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('release_pending');

    expect(await service.reconcileFinancialOperations()).toMatchObject({ failed: 0 });
    const released = await service.resolveDispute(bounty.id, disputed.dispute!.id, resolution);
    expect(released).toMatchObject({
      state: 'released',
      dispute: {
        state: 'released',
        resolution: {
          evidence: {
            summary: 'The merged pull request and its checks meet the written acceptance criteria.',
            references: [pullRequestUrl],
          },
        },
      },
    });
    expect(released.dispute?.resolution?.evidenceHash).toBe(
      fingerprintBountyDisputeEvidence(released.dispute!.resolution!.evidence),
    );
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'bounty_released'),
    ).toHaveLength(1);
    expect(
      (await store.activity(500)).filter((event) => event.kind === 'bounty.dispute_resolved'),
    ).toHaveLength(1);
  });

  it('keeps a refund decision pending until the signer can finalize it', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-dispute-refund',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    let nowMs = Date.now() + 1_000;
    const now = () => new Date(nowMs);
    const policy = new MockPolicy(now);
    const service = new BountyService(
      store,
      policy,
      reviewer({ approved: false, reason: 'not relevant' }),
      now,
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('dispute-refund', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const disputed = await service.openDispute(
      bounty.id,
      contributor,
      'The contribution cannot be completed safely within the accepted scope.',
    );
    const resolution = {
      decision: 'refund' as const,
      evidence: {
        summary: 'The issue record shows the requested work cannot be safely completed as scoped.',
        references: [quote.issueUrl],
      },
      idempotencyKey: 'resolve:dispute-refund',
    };
    policy.failEscrowRefund = true;
    const pending = await service.resolveDispute(bounty.id, disputed.dispute!.id, resolution);
    expect(pending.dispute?.state).toBe('refund_pending');
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('refund_pending');

    policy.failEscrowRefund = false;
    policy.refundNotExpired = true;
    await expect(
      service.resolveDispute(bounty.id, disputed.dispute!.id, resolution),
    ).resolves.toMatchObject({ state: 'disputed', dispute: { state: 'refund_pending' } });

    nowMs = Date.parse(challenge.claimExpiresAt);
    policy.refundNotExpired = false;
    expect(await service.reconcileFinancialOperations()).toMatchObject({ failed: 0 });
    const refunded = await service.resolveDispute(bounty.id, disputed.dispute!.id, resolution);
    expect(refunded).toMatchObject({ state: 'refunded', dispute: { state: 'refunded' } });
    expect(policy.lastRefundReason).toBe('dispute_resolved');
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'bounty_returned'),
    ).toHaveLength(1);
  });

  it('rejects dispute intake after a release authorization has started', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    await store.appendLedger({
      kind: 'treasury_deposit',
      referenceId: 'deposit-dispute-cutoff',
      asset: 'USDC',
      amountAtomic: '200000000',
      amountUsd: 200,
    });
    const policy = new MockPolicy();
    const release = policy.pauseRelease();
    const service = new BountyService(store, policy, reviewer(), tickingClock(), bountyConfig);
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('release-race', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/11';
    await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);

    const releasing = service.releaseMerged(bounty.id, pullRequestUrl);
    await release.started;
    expect((await store.escrowByBounty(bounty.id))?.state).toBe('release_pending');
    await expect(
      service.openDispute(
        bounty.id,
        contributor,
        'A refund must not race an escrow release already submitted for authorization.',
      ),
    ).rejects.toThrow('dispute intake closes');
    release.resume();
    await expect(releasing).resolves.toMatchObject({ state: 'released' });
  });
});

class FailOnceBoundStore extends MemoryStore {
  failNextBound = false;

  override async saveEscrow(escrow: ContributorEscrow): Promise<ContributorEscrow> {
    if (this.failNextBound && escrow.state === 'bound') {
      this.failNextBound = false;
      throw new Error('injected bound write failure');
    }
    return super.saveEscrow(escrow);
  }
}

class FailOnceResolutionStore extends MemoryStore {
  failNextResolution = false;

  override async saveEscrow(escrow: ContributorEscrow): Promise<ContributorEscrow> {
    if (this.failNextResolution && (escrow.state === 'released' || escrow.state === 'refunded')) {
      this.failNextResolution = false;
      throw new Error('injected resolution write failure');
    }
    return super.saveEscrow(escrow);
  }
}

async function refundedJob(store: MemoryStore): Promise<Job> {
  await store.saveQuote(quote);
  const { job } = await store.createJob(
    quote,
    { payer: '3'.repeat(32), transaction: 'settlement', amountAtomic: quote.priceAtomic },
    `key-${Math.random()}`,
  );
  await store.transitionJob(job.id, 'settlement_pending', 'paid');
  await store.transitionJob(job.id, 'paid', 'failed', { error: 'route failed' });
  await store.transitionJob(job.id, 'failed', 'refund_pending');
  return store.transitionJob(job.id, 'refund_pending', 'refunded', {
    refundTransaction: 'refund',
  });
}

function tickingClock(): () => Date {
  let time = Date.parse('2026-08-22T10:00:00Z');
  return () => new Date((time += 1_000));
}

function reviewer(
  decision: { approved: boolean; reason: string } = { approved: true, reason: 'ok' },
  merged: { headSha: string; diffHash: string } = {
    headSha: reviewedHeadSha,
    diffHash: reviewedDiffHash,
  },
): ContributorPatchReviewer {
  return {
    review: async () => ({
      ...decision,
      headSha: reviewedHeadSha,
      baseSha: reviewedBaseSha,
      baseRef: 'main',
      diffHash: reviewedDiffHash,
    }),
    mergedEvidence: async () => ({
      ...merged,
      baseSha: reviewedBaseSha,
      baseRef: 'main',
      mergedAt: '2026-08-22T10:05:00.000Z',
      mergeCommitSha: 'f'.repeat(40),
    }),
  };
}

class MockPolicy implements FinancialPolicy {
  private operation = 0;
  private readonly challengeWallets = new Map<string, string>();
  private readonly resolutionOperations = new Map<string, PolicyOperation>();
  private releasePause?: { started: () => void; gate: Promise<void> };
  failEscrowRefund = false;
  refundNotExpired = false;
  releaseFailuresRemaining = 0;
  escrowRefundRecipient = 'treasury';
  lastRefundReason?: 'expired' | 'rejected' | 'dispute_resolved';
  readonly releaseInputs: Array<{
    pullRequestNumber: number;
    reviewedHeadSha: string;
    reviewedDiffHash: string;
  }> = [];

  constructor(private readonly now?: () => Date) {}

  async refund(): Promise<PolicyOperation> {
    return this.result('refund');
  }

  async readiness() {
    return {
      healthy: true,
      refundTreasury: 'treasury',
      refundMint: 'mint',
      refundDecimals: 6,
      finalizedBalanceRaw: '1000000000',
      pendingRefundRaw: '0',
      treasuryAvailableRefundRaw: '1000000000',
      remainingRefundLimitUsdCents: 100_000,
      availableRefundRaw: '1000000000',
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1000000000',
      availableEscrowReserveLamports: '900000000',
    };
  }

  async registerRefundLiability() {
    throw new Error('not used by bounty tests');
  }

  async dischargeRefundLiability() {
    throw new Error('not used by bounty tests');
  }

  async reserveEscrow(input: { amountUsdCents: number }): Promise<PolicyOperation> {
    return this.result('escrow_reserve', 'vault', input.amountUsdCents);
  }

  async createBindChallenge(
    _reservationId: string,
    input: { claimantWallet: string; githubGrantId: string },
  ) {
    this.operation += 1;
    const id = `00000000-0000-4000-8000-${String(this.operation).padStart(12, '0')}`;
    this.challengeWallets.set(id, input.claimantWallet);
    return {
      id,
      message: `Bind ${input.claimantWallet}`,
      expiresAt: this.now
        ? new Date(this.now().getTime() + 10 * 60_000).toISOString()
        : '2099-01-01T00:00:00.000Z',
      claimExpiresAt: this.now
        ? new Date(this.now().getTime() + 48 * 60 * 60_000).toISOString()
        : '2099-01-03T00:00:00.000Z',
    };
  }

  async bindEscrow(_reservationId: string, challengeId: string): Promise<PolicyOperation> {
    const wallet = this.challengeWallets.get(challengeId);
    if (!wallet) throw new Error('unknown challenge');
    return this.result('escrow_bind', wallet);
  }

  async releaseEscrow(
    operationId: string,
    input: {
      pullRequestNumber: number;
      reviewedHeadSha: string;
      reviewedDiffHash: string;
    },
  ): Promise<PolicyOperation> {
    this.releaseInputs.push(input);
    const key = `${operationId}:release`;
    const existing = this.resolutionOperations.get(key);
    if (existing) return existing;
    if (this.releaseFailuresRemaining > 0) {
      this.releaseFailuresRemaining -= 1;
      throw new Error('release deadline elapsed');
    }
    if (this.releasePause) {
      const pause = this.releasePause;
      pause.started();
      await pause.gate;
      this.releasePause = undefined;
    }
    const operation = this.result('escrow_release', '1'.repeat(32));
    this.resolutionOperations.set(key, operation);
    return operation;
  }

  async refundEscrow(
    operationId: string,
    reason: 'expired' | 'rejected' | 'dispute_resolved',
  ): Promise<PolicyOperation> {
    this.lastRefundReason = reason;
    const key = `${operationId}:refund:${reason}`;
    const existing = this.resolutionOperations.get(key);
    if (existing) return existing;
    if (this.refundNotExpired) throw new Error('Escrow cannot be refunded before expiry');
    if (this.failEscrowRefund) {
      throw new PolicyRequestError('temporary_signer_failure', 503, 'temporary signer failure');
    }
    const operation = this.result('escrow_refund', this.escrowRefundRecipient);
    this.resolutionOperations.set(key, operation);
    return operation;
  }

  pauseRelease(): { started: Promise<void>; resume: () => void } {
    let markStarted!: () => void;
    let resume!: () => void;
    const started = new Promise<void>((resolve) => {
      markStarted = resolve;
    });
    const gate = new Promise<void>((resolve) => {
      resume = resolve;
    });
    this.releasePause = { started: markStarted, gate };
    return { started, resume };
  }

  private result(
    kind: PolicyOperation['kind'],
    recipient = '1'.repeat(32),
    amountUsdCents = 0,
  ): PolicyOperation {
    this.operation += 1;
    return {
      id: `00000000-0000-4000-8000-${String(this.operation).padStart(12, '0')}`,
      kind,
      status: 'finalized',
      amountUsdCents,
      amountAtomic: kind === 'escrow_reserve' ? String(amountUsdCents * 1_000_000) : null,
      asset: 'SOL',
      recipient,
      transactionSignature: `tx-${this.operation}`,
      error: null,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
  }
}

function randomGrantId(): string {
  return `10000000-0000-4000-8000-${String(Math.floor(Math.random() * 1_000_000_000_000)).padStart(12, '0')}`;
}
