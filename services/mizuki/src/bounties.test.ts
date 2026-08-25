import { describe, expect, it } from 'vitest';
import { BountyService, type ContributorPatchReviewer } from './bounties.js';
import { loadConfig } from './config.js';
import { UsePodContributorReviewer } from './contributor-reviewer.js';
import { fingerprintBountyDisputeEvidence, type ContributorEscrow } from './domain/index.js';
import type { GithubClient } from './github.js';
import { PolicyRequestError, type FinancialPolicy, type PolicyOperation } from './policy-client.js';
import { publicBounty } from './public-api.js';
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
    const patchReviewer = reviewer({ approved: true, reason: 'scoped and correct' });
    let reviewCalls = 0;
    const service = new BountyService(
      store,
      policy,
      {
        ...patchReviewer,
        review: async (...args) => {
          reviewCalls += 1;
          return patchReviewer.review(...args);
        },
      },
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
      inputTokens: 20,
      outputTokens: 8,
      provider: {
        model: 'independent-reviewer',
        resolvedModel: 'independent-reviewer-20260825',
        costMicrounits: '1',
      },
    });
    const replay = await service.submitPullRequest(
      created.id,
      contributor,
      'https://github.com/example/project/pull/2',
    );
    expect(replay.validationReceipt?.id).toBe(reviewed.validationReceipt?.id);
    expect(reviewCalls).toBe(1);
    expect(
      (await store.ledgerEntries()).filter(
        (entry) =>
          entry.kind === 'operating_cost' && entry.referenceId.startsWith('bounty-review:'),
      ),
    ).toHaveLength(1);
    const released = await service.releaseMerged(
      created.id,
      'https://github.com/example/project/pull/2',
    );
    expect(released.state).toBe('released');
    expect((await store.escrowByBounty(created.id))?.state).toBe('released');
    expect(policy.releaseInputs).toEqual([
      expect.objectContaining({
        pullRequestNumber: 2,
        reviewedHeadSha,
        reviewedDiffHash,
      }),
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

  it('checkpoints paid provider evidence before rejecting a malformed decision', async () => {
    const store = new ReviewCheckpointStore();
    const job = await refundedJob(store);
    const secret = 'private-provider-diagnostic';
    const github = {
      pullRequestReviewData: async () => ({
        headSha: reviewedHeadSha,
        baseSha: reviewedBaseSha,
        baseRef: 'main',
        diffHash: reviewedDiffHash,
        diff: 'diff --git a/src/parser.ts b/src/parser.ts',
        changedFiles: 1,
        files: [{ filename: 'src/parser.ts', status: 'modified', patchAvailable: true }],
        mergedAt: null,
        mergeCommitSha: null,
        checksPassed: true,
        checkCount: 1,
      }),
    } as unknown as GithubClient;
    const provider = new UsePodContributorReviewer(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        USEPOD_API_KEY: 'funded-token',
        USEPOD_REVIEW_MODEL: 'deepseek-v4-flash',
      }),
      store,
      github,
      async () =>
        Response.json(
          {
            model: 'deepseek-v4-flash-260425',
            choices: [
              {
                index: 0,
                finish_reason: 'stop',
                message: {
                  role: 'assistant',
                  content: `{"approved":true,"approved":false,"reason":"${secret}"}`,
                },
              },
            ],
            usage: { prompt_tokens: 31, completion_tokens: 12 },
          },
          {
            headers: {
              'x-pod-route': 'marketplace',
              'x-balance-remaining': '9000000',
              'x-pod-provider-id': 'provider-3d0',
              'x-request-id': 'provider-request-22',
              'x-balance-cost-microunits': '700',
            },
          },
        ),
    );
    const service = new BountyService(store, new MockPolicy(), provider, tickingClock(), {
      ...bountyConfig,
      usePodModel: 'deepseek-v4-flash',
    });
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('review-checkpoint', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');

    await expect(
      service.submitPullRequest(
        bounty.id,
        contributor,
        'https://github.com/example/project/pull/22',
      ),
    ).rejects.toThrow('UsePod bounty review returned an invalid decision');

    const failed = await store.bounty(bounty.id);
    expect(store.sawReceived).toBe(true);
    expect(failed).toMatchObject({
      state: 'pr_submitted',
      validationAttempt: {
        status: 'failed',
        failureKind: 'provider_error',
        error: 'UsePod bounty review returned an invalid decision',
        inputTokens: 31,
        outputTokens: 12,
        provider: {
          model: 'deepseek-v4-flash',
          resolvedModel: 'deepseek-v4-flash-260425',
          route: 'marketplace',
          providerId: 'provider-3d0',
          requestId: 'provider-request-22',
          costMicrounits: '700',
        },
      },
    });
    expect(JSON.stringify(failed)).not.toContain(secret);
    expect(JSON.stringify(await store.activity())).not.toContain(secret);
    expect(JSON.stringify(await publicBounty(store, failed!))).not.toContain(secret);
  });

  it('persists one paid review attempt before concurrent submissions reach the provider', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    const baseReviewer = reviewer({ approved: true, reason: 'scoped and correct' });
    let reviewCalls = 0;
    let startReview!: () => void;
    let finishReview!: () => void;
    const reviewStarted = new Promise<void>((resolve) => {
      startReview = resolve;
    });
    const reviewGate = new Promise<void>((resolve) => {
      finishReview = resolve;
    });
    const service = new BountyService(
      store,
      new MockPolicy(),
      {
        ...baseReviewer,
        review: async (...args) => {
          reviewCalls += 1;
          startReview();
          await reviewGate;
          return baseReviewer.review(...args);
        },
      },
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('review-race', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/77';

    const first = service.submitPullRequest(bounty.id, contributor, pullRequestUrl);
    await reviewStarted;
    const concurrent = await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);

    expect(concurrent).toMatchObject({
      state: 'validating',
      validationAttempt: { status: 'submitted', maxCostMicrounits: '50000' },
    });
    expect(reviewCalls).toBe(1);
    await expect(service.reconcileFinancialOperations()).resolves.toEqual({
      recovered: 0,
      failed: 0,
    });
    finishReview();
    await expect(first).resolves.toMatchObject({
      state: 'pr_submitted',
      validationAttempt: { status: 'completed' },
    });
    expect(
      (await store.ledgerEntries()).filter(
        (entry) =>
          entry.kind === 'operating_cost' && entry.referenceId.startsWith('bounty-review:'),
      ),
    ).toHaveLength(1);
  });

  it('completes a static rejection without booking provider cost', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    const baseReviewer = reviewer();
    let reviewCalls = 0;
    const service = new BountyService(
      store,
      new MockPolicy(),
      {
        ...baseReviewer,
        preflight: async () => ({
          kind: 'rejected',
          result: {
            approved: false,
            reason: 'repository checks have not passed',
            headSha: reviewedHeadSha,
            baseSha: reviewedBaseSha,
            baseRef: 'main',
            diffHash: reviewedDiffHash,
          },
        }),
        review: async (...args) => {
          reviewCalls += 1;
          return baseReviewer.review(...args);
        },
      },
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('static-rejection', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');

    const rejected = await service.submitPullRequest(
      bounty.id,
      contributor,
      'https://github.com/example/project/pull/82',
    );
    expect(rejected).toMatchObject({
      state: 'pr_submitted',
      validationAttempt: { status: 'completed' },
      validationReceipt: {
        approved: false,
        reason: 'repository checks have not passed',
      },
    });
    expect(rejected.validationReceipt?.provider).toBeUndefined();
    expect(reviewCalls).toBe(0);
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'operating_cost'),
    ).toEqual([]);
  });

  it('uses the durable review cap after a restart with changed configuration', async () => {
    const store = new FailOnceReviewSubmitStore();
    const job = await refundedJob(store);
    const now = tickingClock();
    const initial = new BountyService(store, new MockPolicy(), reviewer(), now, {
      ...bountyConfig,
      bountyReviewMaxCostMicrounits: 50_000,
    });
    const bounty = await initial.createAfterRefund(job);
    const contributor = await store.upsertContributor('review-restart', 'maintainer');
    const challenge = await initial.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await initial.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/79';

    await expect(initial.submitPullRequest(bounty.id, contributor, pullRequestUrl)).rejects.toThrow(
      'injected review submission failure',
    );
    expect(await store.bounty(bounty.id)).toMatchObject({
      state: 'validating',
      validationAttempt: { status: 'reserved', maxCostMicrounits: '50000' },
    });
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'operating_cost'),
    ).toEqual([expect.objectContaining({ amountUsd: 0.05 })]);

    const baseReviewer = reviewer();
    let receivedAttempt: { id: string; maxCostMicrounits: number } | undefined;
    const restarted = new BountyService(
      store,
      new MockPolicy(),
      {
        ...baseReviewer,
        review: async (...args) => {
          receivedAttempt = args[0].attempt;
          return baseReviewer.review(...args);
        },
      },
      now,
      { ...bountyConfig, bountyReviewMaxCostMicrounits: 100_000 },
    );

    await expect(
      restarted.submitPullRequest(bounty.id, contributor, pullRequestUrl),
    ).resolves.toMatchObject({
      validationAttempt: { status: 'completed', maxCostMicrounits: '50000' },
    });
    expect(receivedAttempt).toMatchObject({ maxCostMicrounits: 50_000 });
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'operating_cost'),
    ).toEqual([expect.objectContaining({ amountUsd: 0.05 })]);
  });

  it('fails closed before booking or review when a durable cap is invalid', async () => {
    const store = new FailOnceReviewLedgerStore();
    const job = await refundedJob(store);
    const service = new BountyService(store, new MockPolicy(), reviewer(), tickingClock(), {
      ...bountyConfig,
      bountyReviewMaxCostMicrounits: 50_000,
    });
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('invalid-review-cap', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/80';
    await expect(service.submitPullRequest(bounty.id, contributor, pullRequestUrl)).rejects.toThrow(
      'injected review ledger failure',
    );

    const reserved = (await store.bounty(bounty.id))!;
    await store.updateBounty(
      {
        ...reserved,
        validationAttempt: { ...reserved.validationAttempt!, maxCostMicrounits: '1000001' },
        updatedAt: new Date().toISOString(),
        revision: reserved.revision + 1,
      },
      reserved.revision,
    );
    const paidReview = reviewer();
    let reviewCalls = 0;
    const restarted = new BountyService(
      store,
      new MockPolicy(),
      {
        ...paidReview,
        review: async (...args) => {
          reviewCalls += 1;
          return paidReview.review(...args);
        },
      },
      tickingClock(),
      { ...bountyConfig, bountyReviewMaxCostMicrounits: 100_000 },
    );

    await expect(
      restarted.submitPullRequest(bounty.id, contributor, pullRequestUrl),
    ).rejects.toThrow('invalid durable cost reservation');
    expect(reviewCalls).toBe(0);
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'operating_cost'),
    ).toEqual([]);
  });

  it('reports provider and terminal checkpoint failures without retrying the paid review', async () => {
    const store = new FailValidationCheckpointStore();
    const job = await refundedJob(store);
    const baseReviewer = reviewer();
    let reviewCalls = 0;
    const service = new BountyService(
      store,
      new MockPolicy(),
      {
        ...baseReviewer,
        review: async () => {
          reviewCalls += 1;
          throw new Error('provider validation failed');
        },
      },
      tickingClock(),
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('dual-review-failure', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/81';

    let failure: unknown;
    try {
      await service.submitPullRequest(bounty.id, contributor, pullRequestUrl);
    } catch (error) {
      failure = error;
    }

    expect(failure).toBeInstanceOf(AggregateError);
    expect(failure).toMatchObject({
      message: 'bounty review provider and terminal checkpoint both failed',
      errors: [
        expect.objectContaining({ message: 'provider validation failed' }),
        expect.objectContaining({ message: 'injected terminal checkpoint failure' }),
      ],
    });
    expect(await store.bounty(bounty.id)).toMatchObject({
      state: 'validating',
      validationAttempt: { status: 'submitted' },
    });

    await expect(
      service.submitPullRequest(bounty.id, contributor, pullRequestUrl),
    ).resolves.toMatchObject({
      state: 'validating',
      validationAttempt: { status: 'submitted' },
    });
    expect(reviewCalls).toBe(1);
  });

  it('terminalizes a stale received review after restart without another provider call', async () => {
    const store = new FailOnceReviewCompletionStore();
    const job = await refundedJob(store);
    let nowMs = Date.parse('2026-08-22T10:00:00Z');
    const now = () => new Date((nowMs += 1_000));
    const initialReviewer = reviewer();
    let initialReviewCalls = 0;
    const initial = new BountyService(
      store,
      new MockPolicy(),
      {
        ...initialReviewer,
        review: async (preflight, checkpoint) => {
          initialReviewCalls += 1;
          const result = await initialReviewer.review(preflight);
          await checkpoint?.({
            providerReceipt: result.providerReceipt!,
            inputTokens: result.inputTokens,
            outputTokens: result.outputTokens,
          });
          return result;
        },
      },
      now,
      bountyConfig,
    );
    const bounty = await initial.createAfterRefund(job);
    const contributor = await store.upsertContributor('review-crash', 'maintainer');
    const challenge = await initial.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await initial.claim(bounty.id, contributor, challenge.id, 'signature');
    const pullRequestUrl = 'https://github.com/example/project/pull/83';

    await expect(initial.submitPullRequest(bounty.id, contributor, pullRequestUrl)).rejects.toThrow(
      'injected review completion failure',
    );
    expect(initialReviewCalls).toBe(1);
    expect(await store.bounty(bounty.id)).toMatchObject({
      state: 'validating',
      validationAttempt: {
        status: 'received',
        inputTokens: 20,
        outputTokens: 8,
        provider: { requestId: 'review-request', costMicrounits: '1' },
      },
    });
    expect(
      (await store.ledgerEntries()).filter((entry) => entry.kind === 'operating_cost'),
    ).toEqual([expect.objectContaining({ amountUsd: 0.05 })]);

    nowMs += 2 * 60_000;
    const restartedReviewer = reviewer();
    let restartedReviewCalls = 0;
    const restarted = new BountyService(
      store,
      new MockPolicy(),
      {
        ...restartedReviewer,
        review: async (...args) => {
          restartedReviewCalls += 1;
          return restartedReviewer.review(...args);
        },
      },
      now,
      bountyConfig,
    );

    await expect(restarted.reconcileFinancialOperations()).resolves.toEqual({
      recovered: 1,
      failed: 0,
    });
    expect(await store.bounty(bounty.id)).toMatchObject({
      state: 'pr_submitted',
      validationAttempt: {
        status: 'failed',
        failureKind: 'indeterminate_after_recovery',
        error: expect.stringContaining('will not be retried'),
        inputTokens: 20,
        outputTokens: 8,
        provider: { requestId: 'review-request', costMicrounits: '1' },
      },
    });
    await expect(
      restarted.submitPullRequest(bounty.id, contributor, pullRequestUrl),
    ).resolves.toMatchObject({ validationAttempt: { status: 'failed' } });
    expect(restartedReviewCalls).toBe(0);
  });

  it('refunds a submitted review attempt that remains ambiguous at claim expiry', async () => {
    const store = new MemoryStore();
    const job = await refundedJob(store);
    let nowMs = Date.now() + 1_000;
    const now = () => new Date((nowMs += 1_000));
    const policy = new MockPolicy(now);
    const baseReviewer = reviewer({ approved: true, reason: 'late response' });
    let startReview!: () => void;
    let finishReview!: () => void;
    const reviewStarted = new Promise<void>((resolve) => {
      startReview = resolve;
    });
    const reviewGate = new Promise<void>((resolve) => {
      finishReview = resolve;
    });
    const service = new BountyService(
      store,
      policy,
      {
        ...baseReviewer,
        review: async (...args) => {
          startReview();
          await reviewGate;
          return baseReviewer.review(...args);
        },
      },
      now,
      bountyConfig,
    );
    const bounty = await service.createAfterRefund(job);
    const contributor = await store.upsertContributor('ambiguous-review', 'maintainer');
    const challenge = await service.createClaimChallenge(
      bounty.id,
      contributor,
      '1'.repeat(32),
      randomGrantId(),
    );
    await service.claim(bounty.id, contributor, challenge.id, 'signature');
    const pendingReview = service.submitPullRequest(
      bounty.id,
      contributor,
      'https://github.com/example/project/pull/78',
    );
    await reviewStarted;
    expect(await store.bounty(bounty.id)).toMatchObject({
      state: 'validating',
      validationAttempt: { status: 'submitted' },
    });

    nowMs = Date.parse(challenge.claimExpiresAt);
    expect(await service.expireClaims()).toBe(1);
    expect(await store.bounty(bounty.id)).toMatchObject({ state: 'refunded' });
    expect(await store.escrowByBounty(bounty.id)).toMatchObject({ state: 'refunded' });

    finishReview();
    await expect(pendingReview).rejects.toThrow('concurrent update');
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

class ReviewCheckpointStore extends MemoryStore {
  sawReceived = false;

  override async updateBounty(...args: Parameters<MemoryStore['updateBounty']>) {
    if (args[0].validationAttempt?.status === 'received') this.sawReceived = true;
    return super.updateBounty(...args);
  }
}

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

class FailOnceReviewLedgerStore extends MemoryStore {
  private failNextReviewLedger = true;

  override async appendLedger(entry: Parameters<MemoryStore['appendLedger']>[0]) {
    if (this.failNextReviewLedger && entry.kind === 'operating_cost') {
      this.failNextReviewLedger = false;
      throw new Error('injected review ledger failure');
    }
    return super.appendLedger(entry);
  }
}

class FailOnceReviewSubmitStore extends MemoryStore {
  private failNextReviewSubmission = true;

  override async updateBounty(
    bounty: Parameters<MemoryStore['updateBounty']>[0],
    expectedRevision: number,
  ) {
    if (this.failNextReviewSubmission && bounty.validationAttempt?.status === 'submitted') {
      this.failNextReviewSubmission = false;
      throw new Error('injected review submission failure');
    }
    return super.updateBounty(bounty, expectedRevision);
  }
}

class FailValidationCheckpointStore extends MemoryStore {
  override async updateBounty(
    bounty: Parameters<MemoryStore['updateBounty']>[0],
    expectedRevision: number,
  ) {
    if (bounty.validationAttempt?.status === 'failed') {
      throw new Error('injected terminal checkpoint failure');
    }
    return super.updateBounty(bounty, expectedRevision);
  }
}

class FailOnceReviewCompletionStore extends MemoryStore {
  private failNextReviewCompletion = true;

  override async updateBounty(
    bounty: Parameters<MemoryStore['updateBounty']>[0],
    expectedRevision: number,
  ) {
    if (this.failNextReviewCompletion && bounty.validationAttempt?.status === 'completed') {
      this.failNextReviewCompletion = false;
      throw new Error('injected review completion failure');
    }
    return super.updateBounty(bounty, expectedRevision);
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
  const evidence = {
    headSha: reviewedHeadSha,
    baseSha: reviewedBaseSha,
    baseRef: 'main',
    diffHash: reviewedDiffHash,
  };
  return {
    preflight: async (_bounty, _pullRequestUrl, attempt) => ({
      kind: 'paid',
      attempt,
      evidence,
      providerInput: {
        model: 'independent-reviewer',
        issue: { title: quote.issueTitle, body: quote.issueBody },
        diff: 'diff --git a/src/parser.ts b/src/parser.ts',
        repositoryChecks: { count: 1, passed: true },
        maxOutputTokens: 512,
      },
    }),
    review: async () => ({
      ...decision,
      ...evidence,
      providerReceipt: {
        model: 'independent-reviewer',
        resolvedModel: 'independent-reviewer-20260825',
        route: 'marketplace',
        requestId: 'review-request',
        costMicrounits: '1',
      },
      inputTokens: 20,
      outputTokens: 8,
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
    repository: string;
    issueNumber: number;
    pullRequestNumber: number;
    mergeCommitSha: string;
    reviewedHeadSha: string;
    reviewedBaseSha: string;
    reviewedBaseRef: string;
    reviewedDiffHash: string;
    reviewReceiptId: string;
    reviewReceiptHash: string;
    reviewModel: string;
    reviewRoute: 'marketplace';
    reviewedAt: string;
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
      remainingEscrowLimitUsdCents: 100_000,
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1000000000',
      availableEscrowReserveLamports: '900000000',
    };
  }

  async assertRepositoryReady(repository: string) {
    return {
      ready: true as const,
      repository: repository.toLowerCase(),
      verifierAppId: '2',
      installationId: 1,
      repositorySelection: 'selected' as const,
      permissions: {
        checks: 'read' as const,
        contents: 'read' as const,
        issues: 'read' as const,
        metadata: 'read' as const,
        pull_requests: 'read' as const,
        statuses: 'read' as const,
      },
      tokenRepositories: 1 as const,
      tokenExpiresAt: '2099-01-01T00:00:00.000Z',
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
      repository: string;
      issueNumber: number;
      pullRequestNumber: number;
      mergeCommitSha: string;
      reviewedHeadSha: string;
      reviewedBaseSha: string;
      reviewedBaseRef: string;
      reviewedDiffHash: string;
      reviewReceiptId: string;
      reviewReceiptHash: string;
      reviewModel: string;
      reviewRoute: 'marketplace';
      reviewedAt: string;
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
