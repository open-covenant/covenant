import { createHash, randomUUID } from 'node:crypto';
import type { Config } from './config.js';
import {
  beginRescueBountyDisputeResolution,
  claimRescueBounty,
  createContributorEscrow,
  createRescueBounty,
  expireAcceptedRescueBountyRelease,
  expireRescueBountyOffer,
  expireRescueBountyClaim,
  fingerprintBountyDisputeEvidence,
  finalizeRescueBountyDisputeResolution,
  finalizeExpiredReleaseRefund,
  finalizeRescueBountyOfferRefund,
  finalizeRescueBountyClaimRefund,
  finalizeContributorEscrowBinding,
  handoffRescueBountyDisputeReleaseToRefund,
  normalizeBountyDisputeEvidence,
  openRescueBountyDispute,
  prepareContributorEscrowBinding,
  submitDraftPullRequest,
  transitionContributorEscrow,
  transitionRescueBounty,
  type BountyDisputeDecision,
  type BountyDisputeEvidence,
  type BountyValidationAttempt,
  type ContributorEscrow,
  type RescueBounty,
} from './domain/index.js';
import {
  PendingPolicyOperationError,
  PolicyRequestError,
  type BindChallenge,
  type FinancialPolicy,
  type PolicyOperation,
} from './policy-client.js';
import type { MizukiStore } from './store.js';
import type { Contributor, Job, ProviderRouteReceipt } from './types.js';

export type ContributorPatchReviewAttempt = { id: string; maxCostMicrounits: number };

export type ContributorPatchReviewAccounting = {
  providerReceipt: ProviderRouteReceipt;
  inputTokens?: number;
  outputTokens?: number;
};

export class ContributorPatchReviewError extends Error {
  constructor(
    message: string,
    readonly accounting: ContributorPatchReviewAccounting,
  ) {
    super(message);
    this.name = 'ContributorPatchReviewError';
  }
}

export type ContributorPatchReviewEvidence = {
  headSha: string;
  baseSha: string;
  baseRef: string;
  diffHash: string;
};

export type ContributorPatchReviewResult = ContributorPatchReviewEvidence & {
  approved: boolean;
  reason: string;
  providerReceipt?: ProviderRouteReceipt;
  inputTokens?: number;
  outputTokens?: number;
};

export type ContributorPatchPaidPreflight = {
  kind: 'paid';
  attempt: ContributorPatchReviewAttempt;
  evidence: ContributorPatchReviewEvidence;
  providerInput: {
    model: string;
    issue: { title: string; body: string };
    diff: string;
    repositoryChecks: { count: number; passed: boolean };
    maxOutputTokens: number;
  };
};

export type ContributorPatchReviewPreflight =
  | { kind: 'rejected'; result: ContributorPatchReviewResult & { approved: false } }
  | ContributorPatchPaidPreflight;

export interface ContributorPatchReviewer {
  preflight(
    bounty: RescueBounty,
    pullRequestUrl: string,
    attempt: ContributorPatchReviewAttempt,
  ): Promise<ContributorPatchReviewPreflight>;
  review(
    preflight: ContributorPatchPaidPreflight,
    checkpoint?: (accounting: ContributorPatchReviewAccounting) => Promise<void>,
  ): Promise<ContributorPatchReviewResult>;
  mergedEvidence(
    bounty: RescueBounty,
    pullRequestUrl: string,
  ): Promise<{
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    mergedAt: string;
    mergeCommitSha: string;
  }>;
}

export type DisputeResolutionInput = {
  decision: BountyDisputeDecision;
  evidence: BountyDisputeEvidence;
  idempotencyKey: string;
};

const MAX_BOUNTY_REVIEW_COST_MICROUNITS = 1_000_000;
const SUBMITTED_REVIEW_STALE_MS = 2 * 60_000;

export class BountyService {
  private readonly refundRecipient: string;
  private readonly reviewMaxCostMicrounits: number;
  private readonly reviewModel: string;

  constructor(
    private readonly store: MizukiStore,
    private readonly policy: FinancialPolicy,
    private readonly reviewer: ContributorPatchReviewer,
    private readonly now: () => Date = () => new Date(),
    config: Pick<Config, 'escrowRefundTo'> &
      Partial<Pick<Config, 'bountyReviewMaxCostMicrounits' | 'usePodModel'>> = {
      escrowRefundTo: '',
    },
  ) {
    this.refundRecipient = config.escrowRefundTo;
    this.reviewMaxCostMicrounits = config.bountyReviewMaxCostMicrounits ?? 50_000;
    this.reviewModel = config.usePodModel ?? 'independent-reviewer';
  }

  async createAfterRefund(job: Job): Promise<RescueBounty> {
    if (job.state !== 'refunded') throw new Error('rescue bounty requires a completed refund');
    const latest = await this.store.bountyBySourceJob(job.id);
    if (latest) {
      if (
        latest.state !== 'refunded' ||
        !latest.claimHistory.some((claim) => claim.state === 'expired')
      ) {
        return ['draft', 'awaiting_funding', 'funding'].includes(latest.state)
          ? this.fund(latest)
          : latest;
      }
      return this.createGeneration(job, latest.generation + 1, latest.id);
    }
    return this.createGeneration(job, 0);
  }

  private async createGeneration(
    job: Job,
    generation: number,
    predecessorBountyId?: string,
  ): Promise<RescueBounty> {
    const at = this.now().toISOString();
    const bounty = createRescueBounty({
      id: randomUUID(),
      sourceJobId: job.id,
      failureReceiptId: `failure:${job.id}`,
      repository: `${job.quote.owner}/${job.quote.repo}`,
      issueNumber: job.quote.issueNumber,
      issueUrl: job.quote.issueUrl,
      jobPriceCents: Number(job.payment.amountAtomic) / 10_000,
      generation,
      predecessorBountyId,
      at,
    });
    const created = await this.store.createBounty(bounty);
    if (!created.created) {
      return ['draft', 'awaiting_funding', 'funding'].includes(created.bounty.state)
        ? this.fund(created.bounty)
        : created.bounty;
    }
    await this.store.appendActivity('bounty.created', bounty.id, {
      sourceJobId: job.id,
      issueUrl: bounty.issueUrl,
      priceCents: bounty.priceCents,
    });
    return this.fund(created.bounty);
  }

  async fundAwaiting(): Promise<void> {
    for (const bounty of await this.store.bountiesList()) {
      if (!['awaiting_funding', 'draft', 'funding'].includes(bounty.state)) continue;
      await this.fund(bounty);
    }
  }

  async createClaimChallenge(
    bountyId: string,
    contributor: Contributor,
    wallet: string,
    githubGrantId: string,
  ): Promise<BindChallenge> {
    const bounty = await this.required(bountyId);
    if (bounty.state !== 'open' || bounty.activeClaim) {
      throw new Error('bounty is not accepting claims');
    }
    const escrow = await this.store.escrowByBounty(bounty.id);
    if (escrow?.state !== 'funded' || !escrow.reservationId) {
      throw new Error('bounty escrow is not funded');
    }
    const challenge = await this.policy.createBindChallenge(escrow.reservationId, {
      claimantWallet: wallet,
      githubGrantId,
    });
    await this.store.saveWalletChallenge({
      id: challenge.id,
      githubId: contributor.githubId,
      wallet,
      message: challenge.message,
      kind: 'bounty_bind',
      bountyId: bounty.id,
      reservationId: escrow.reservationId,
      claimExpiresAt: challenge.claimExpiresAt,
      expiresAt: challenge.expiresAt,
      createdAt: this.now().toISOString(),
    });
    return challenge;
  }

  async claim(
    bountyId: string,
    contributor: Contributor,
    challengeId: string,
    signature: string,
  ): Promise<RescueBounty> {
    const challenge = await this.store.walletChallenge(challengeId, contributor.githubId);
    if (
      !challenge ||
      challenge.kind !== 'bounty_bind' ||
      challenge.bountyId !== bountyId ||
      !challenge.reservationId ||
      !challenge.claimExpiresAt
    ) {
      throw new Error('bounty wallet challenge not found');
    }
    let bounty = await this.required(bountyId);
    if (
      bounty.activeClaim?.id === challenge.id &&
      bounty.activeClaim.claimantId === contributor.githubId &&
      ['claimed', 'pr_submitted', 'validating', 'accepted', 'released'].includes(bounty.state)
    ) {
      return bounty;
    }
    let escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow?.reservationId || escrow.reservationId !== challenge.reservationId) {
      throw new Error('bounty escrow reservation does not match the challenge');
    }
    if (escrow.state === 'funded') {
      escrow = prepareContributorEscrowBinding(escrow, {
        claimId: challenge.id,
        claimantId: contributor.githubId,
        claimantGithubLogin: contributor.githubLogin,
        recipientWallet: challenge.wallet,
        claimExpiresAt: challenge.claimExpiresAt,
        signature,
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    if (escrow.state === 'bind_pending') escrow = await this.completeBinding(escrow);
    if (
      escrow.state !== 'bound' ||
      escrow.claimId !== challenge.id ||
      escrow.claimantId !== contributor.githubId ||
      escrow.recipientWallet !== challenge.wallet ||
      escrow.claimExpiresAt !== challenge.claimExpiresAt
    ) {
      throw new Error('escrow is bound to a different claimant');
    }
    bounty = await this.required(bountyId);
    if (bounty.state !== 'open') throw new Error('bounty is not accepting claims');
    const claimed = claimRescueBounty(bounty, {
      claimId: challenge.id,
      claimantId: contributor.githubId,
      walletAddress: challenge.wallet,
      leaseExpiresAt: challenge.claimExpiresAt,
      at: this.now().toISOString(),
      expectedRevision: bounty.revision,
    });
    await this.store.updateBounty(claimed, bounty.revision);
    await this.store.appendActivity('bounty.claimed', bounty.id, {
      githubLogin: contributor.githubLogin,
      leaseExpiresAt: claimed.activeClaim?.leaseExpiresAt,
    });
    await this.store.consumeWalletChallenge(challenge.id, contributor.githubId);
    return claimed;
  }

  async submitPullRequest(
    bountyId: string,
    contributor: Contributor,
    pullRequestUrl: string,
  ): Promise<RescueBounty> {
    let bounty = await this.required(bountyId);
    if (bounty.activeClaim?.claimantId !== contributor.githubId) {
      throw new Error('only the active claimant may submit a pull request');
    }
    if (
      bounty.activeClaim?.draftPullRequestUrl === pullRequestUrl &&
      (bounty.state === 'pr_submitted' || bounty.state === 'validating')
    ) {
      if (bounty.validationReceipt || bounty.validationAttempt?.status === 'failed') return bounty;
      return this.validate(bounty);
    }
    const submitted = submitDraftPullRequest(bounty, {
      pullRequestUrl,
      at: this.now().toISOString(),
      expectedRevision: bounty.revision,
    });
    bounty = await this.store.updateBounty(submitted, bounty.revision);
    await this.store.appendActivity('bounty.pr_submitted', bounty.id, { pullRequestUrl });
    return this.validate(bounty);
  }

  async releaseMerged(bountyId: string, pullRequestUrl: string): Promise<RescueBounty> {
    let bounty = await this.required(bountyId);
    if (bounty.state === 'released') return bounty;
    if (bounty.state === 'refunded') return bounty;
    if (bounty.activeClaim?.draftPullRequestUrl !== pullRequestUrl) {
      throw new Error('merged pull request does not match the active claim');
    }
    if (bounty.state === 'release_refund_pending') {
      return this.settleExpiredRelease(bounty, pullRequestUrl);
    }
    if (!bounty.validationReceipt?.approved) bounty = await this.validate(bounty);
    if (!bounty.validationReceipt?.approved) {
      throw new Error('merged pull request has not passed independent review');
    }
    const merge = await this.reviewer.mergedEvidence(bounty, pullRequestUrl);
    assertReviewedMerge(bounty, merge);
    if (bounty.state !== 'accepted') {
      const review = bounty.validationReceipt;
      const validating = transitionRescueBounty(bounty, 'validating', {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      bounty = await this.store.updateBounty(validating, bounty.revision);
      const accepted = transitionRescueBounty(bounty, 'accepted', {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      bounty = await this.store.updateBounty(accepted, bounty.revision);
      await this.store.appendActivity('bounty.accepted', bounty.id, {
        pullRequestUrl,
        headSha: merge.headSha,
        diffHash: merge.diffHash,
        reviewedBaseSha: review.baseSha,
        reviewedBaseRef: review.baseRef,
        mergedBaseSha: merge.baseSha,
        mergedBaseRef: merge.baseRef,
        mergeCommitSha: merge.mergeCommitSha,
        mergedAt: merge.mergedAt,
      });
    }

    let escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow?.reservationId || !escrow.recipientWallet) {
      throw new Error('contributor escrow is not bound');
    }
    if (escrow.state === 'bound') {
      escrow = transitionContributorEscrow(escrow, 'release_pending', {
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    if (escrow.state === 'release_pending') {
      if (!escrow.reservationId) throw new Error('escrow reservation is missing');
      let operation: PolicyOperation;
      try {
        operation = await this.policy.releaseEscrow(
          escrow.reservationId,
          releaseEvidence(bounty, pullRequestUrl, merge),
        );
      } catch (cause) {
        if (!this.claimDeadlineReached(escrow)) throw cause;
        return this.settleExpiredRelease(bounty, pullRequestUrl, escrow);
      }
      assertOperation(operation, 'escrow_release');
      if (operation.recipient !== escrow.recipientWallet) {
        throw new Error('escrow release recipient does not match the verified claimant wallet');
      }
      if (!operation.transactionSignature) throw new Error('escrow release has no transaction');
      escrow = transitionContributorEscrow(escrow, 'released', {
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
        transactionSignature: operation.transactionSignature,
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    return this.finalizeReleasedBounty(bounty, escrow, pullRequestUrl);
  }

  async expireClaims(): Promise<number> {
    let expired = 0;
    for (const bounty of await this.store.bountiesList()) {
      if (
        bounty.state === 'accepted' &&
        bounty.activeClaim?.draftPullRequestUrl &&
        Date.parse(bounty.activeClaim.leaseExpiresAt) <= this.now().getTime()
      ) {
        try {
          await this.releaseMerged(bounty.id, bounty.activeClaim.draftPullRequestUrl);
        } catch {
          // Release/refund state is durable and reconciliation retries it.
        }
        expired += 1;
        continue;
      }
      if (!bounty.activeClaim || !['claimed', 'pr_submitted', 'validating'].includes(bounty.state))
        continue;
      if (Date.parse(bounty.activeClaim.leaseExpiresAt) > this.now().getTime()) continue;
      const pending = expireRescueBountyClaim(bounty, {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      await this.store.updateBounty(pending, bounty.revision);
      await this.store.appendActivity('bounty.expired', bounty.id, {});
      try {
        await this.settleExpiredClaim(pending);
      } catch {
        // The durable pending state is retried by reconcileFinancialOperations.
      }
      expired += 1;
    }
    return expired;
  }

  async expireOffers(): Promise<number> {
    let expired = 0;
    for (const bounty of await this.store.bountiesList()) {
      if (bounty.state !== 'open' || bounty.activeClaim) continue;
      if (Date.parse(bounty.offerExpiresAt) > this.now().getTime()) continue;
      const pending = expireRescueBountyOffer(bounty, {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      await this.store.updateBounty(pending, bounty.revision);
      try {
        await this.settleExpiredOffer(pending);
      } catch {
        // The durable pending state is retried by reconcileFinancialOperations.
      }
      expired += 1;
    }
    return expired;
  }

  async reconcileFinancialOperations(): Promise<{ recovered: number; failed: number }> {
    let recovered = 0;
    let failed = 0;
    for (const bounty of await this.store.bountiesList()) {
      try {
        if (['draft', 'awaiting_funding', 'funding'].includes(bounty.state)) {
          await this.fund(bounty);
          recovered += 1;
          continue;
        }
        let escrow = await this.store.escrowByBounty(bounty.id);
        if (!escrow) continue;

        if (
          (bounty.state === 'released' || bounty.state === 'refunded') &&
          bounty.dispute?.resolution?.resolvedAt
        ) {
          await this.publishDisputeResolution(bounty);
          recovered += 1;
          continue;
        }

        if (bounty.state === 'open' && escrow.state === 'bind_pending') {
          escrow = await this.completeBinding(escrow);
          await this.recoverBoundClaim(bounty, escrow);
          recovered += 1;
          continue;
        }

        if (bounty.state === 'open' && escrow.state === 'bound') {
          await this.recoverBoundClaim(bounty, escrow);
          recovered += 1;
          continue;
        }

        if (bounty.state === 'validating' && bounty.activeClaim?.draftPullRequestUrl) {
          if (
            bounty.validationAttempt &&
            ['submitted', 'received'].includes(bounty.validationAttempt.status) &&
            !submittedReviewIsStale(bounty.validationAttempt, this.now())
          ) {
            continue;
          }
          await this.validate(bounty, true);
          recovered += 1;
          continue;
        }
        if (bounty.state === 'disputed' && bounty.dispute) {
          await this.publishDisputeOpened(bounty);
          await this.ensureEscrowDisputed(bounty, escrow);
          if (bounty.dispute.state !== 'open') {
            await this.settleDisputeResolution(await this.required(bounty.id));
          }
          recovered += 1;
          continue;
        }
        if (bounty.state === 'claim_refund_pending') {
          await this.settleExpiredClaim(await this.required(bounty.id));
          recovered += 1;
          continue;
        }
        if (bounty.state === 'offer_refund_pending') {
          await this.settleExpiredOffer(await this.required(bounty.id));
          recovered += 1;
          continue;
        }
        if (bounty.state === 'release_refund_pending' && bounty.activeClaim?.draftPullRequestUrl) {
          await this.settleExpiredRelease(bounty, bounty.activeClaim.draftPullRequestUrl, escrow);
          recovered += 1;
          continue;
        }
        if (
          bounty.state === 'accepted' &&
          bounty.activeClaim?.draftPullRequestUrl &&
          ['bound', 'release_pending', 'released'].includes(escrow.state)
        ) {
          await this.releaseMerged(bounty.id, bounty.activeClaim.draftPullRequestUrl);
          recovered += 1;
        }
      } catch {
        failed += 1;
      }
    }
    return { recovered, failed };
  }

  async openDispute(
    bountyId: string,
    contributor: Contributor,
    reason: string,
  ): Promise<RescueBounty> {
    let bounty = await this.required(bountyId);
    if (bounty.activeClaim?.claimantId !== contributor.githubId) {
      throw new Error('only the active claimant may dispute a bounty');
    }
    const normalizedReason = reason.trim();
    if (normalizedReason.length < 10 || normalizedReason.length > 2_000) {
      throw new Error('dispute reason must contain 10-2000 characters');
    }
    let escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow?.reservationId || !escrow.recipientWallet) {
      throw new Error('contributor escrow is not bound');
    }

    if (bounty.state === 'disputed') {
      if (
        !bounty.dispute ||
        bounty.dispute.claimantId !== contributor.githubId ||
        bounty.dispute.reason !== normalizedReason
      ) {
        throw new Error('bounty already has a different dispute');
      }
      await this.publishDisputeOpened(bounty);
      await this.ensureEscrowDisputed(bounty, escrow);
      return bounty;
    }
    if (!['claimed', 'pr_submitted', 'validating'].includes(bounty.state)) {
      throw new Error('dispute intake closes before escrow release authorization begins');
    }
    if (escrow.state !== 'bound') {
      throw new Error(`escrow can no longer be disputed from ${escrow.state}`);
    }

    const disputeId = escrow.disputeId ?? randomUUID();
    const disputed = openRescueBountyDispute(bounty, {
      disputeId,
      claimantId: contributor.githubId,
      reason: normalizedReason,
      at: this.now().toISOString(),
      expectedRevision: bounty.revision,
    });
    bounty = await this.store.updateBounty(disputed, bounty.revision);
    await this.publishDisputeOpened(bounty);
    escrow = await this.ensureEscrowDisputed(bounty, escrow);
    if (escrow.state !== 'disputed') throw new Error('escrow dispute checkpoint is incomplete');
    return bounty;
  }

  async resolveDispute(
    bountyId: string,
    disputeId: string,
    input: DisputeResolutionInput,
  ): Promise<RescueBounty> {
    let bounty = await this.required(bountyId);
    if (!bounty.dispute || bounty.dispute.id !== disputeId) {
      throw new Error('dispute not found for bounty');
    }
    const evidence = normalizeBountyDisputeEvidence(input.evidence);
    const evidenceHash = fingerprintBountyDisputeEvidence(evidence);
    const resolution = bounty.dispute.resolution;
    if (resolution) {
      if (
        resolution.idempotencyKey !== input.idempotencyKey.trim() ||
        resolution.requestedDecision !== input.decision ||
        resolution.evidenceHash !== evidenceHash
      ) {
        throw new Error('dispute already has a different resolution');
      }
      if (bounty.state === 'released' || bounty.state === 'refunded') {
        await this.publishDisputeResolution(bounty);
        return bounty;
      }
    } else {
      if (input.decision === 'release') this.assertDisputeReleaseCanStart(bounty, evidence);
      const pending = beginRescueBountyDisputeResolution(bounty, {
        disputeId,
        resolutionId: randomUUID(),
        idempotencyKey: input.idempotencyKey,
        decision: input.decision,
        evidence,
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      bounty = await this.store.updateBounty(pending, bounty.revision);
    }
    return this.settleDisputeResolution(bounty);
  }

  private async ensureEscrowDisputed(
    bounty: RescueBounty,
    initial?: ContributorEscrow,
  ): Promise<ContributorEscrow> {
    const dispute = bounty.dispute;
    if (!dispute) throw new Error('bounty dispute metadata is missing');
    let escrow = initial ?? (await this.store.escrowByBounty(bounty.id));
    if (!escrow?.reservationId || !escrow.recipientWallet) {
      throw new Error('disputed contributor escrow is incomplete');
    }
    if (escrow.disputeId && escrow.disputeId !== dispute.id) {
      throw new Error('escrow belongs to a different dispute');
    }
    if (
      escrow.state === 'disputed' ||
      (dispute.state === 'release_pending' && escrow.state === 'release_pending') ||
      (dispute.state === 'refund_pending' && escrow.state === 'refund_pending') ||
      escrow.state === 'released' ||
      escrow.state === 'refunded'
    ) {
      return escrow;
    }
    escrow = transitionContributorEscrow(escrow, 'disputed', {
      disputeId: dispute.id,
      at: this.now().toISOString(),
      expectedRevision: escrow.revision,
    });
    return this.store.saveEscrow(escrow);
  }

  private assertDisputeReleaseCanStart(
    bounty: RescueBounty,
    evidence: BountyDisputeEvidence,
  ): void {
    const pullRequestUrl = bounty.activeClaim?.draftPullRequestUrl;
    if (!pullRequestUrl) {
      throw new Error('release resolution requires the active claim pull request');
    }
    if (!evidence.references.includes(pullRequestUrl)) {
      throw new Error('release evidence must reference the active claim pull request');
    }
    assertReleaseEvidenceAvailable(bounty, pullRequestUrl);
    if (
      !bounty.activeClaim?.leaseExpiresAt ||
      Date.parse(bounty.activeClaim.leaseExpiresAt) <= this.now().getTime()
    ) {
      throw new Error('release resolution cannot start after the immutable claim deadline');
    }
  }

  private async settleDisputeResolution(initial: RescueBounty): Promise<RescueBounty> {
    let bounty = await this.required(initial.id);
    const dispute = bounty.dispute;
    const resolution = dispute?.resolution;
    if (!dispute || !resolution || dispute.state === 'open') {
      throw new Error('dispute resolution has not been decided');
    }
    if (bounty.state === 'released' || bounty.state === 'refunded') {
      await this.publishDisputeResolution(bounty);
      return bounty;
    }
    let escrow = await this.ensureEscrowDisputed(bounty);
    if (escrow.state === 'released') {
      return this.finalizeDisputeResolution(bounty, escrow, 'release');
    }
    if (escrow.state === 'refunded') {
      return this.finalizeDisputeResolution(bounty, escrow, 'refund');
    }

    if (resolution.settlementDecision === 'release') {
      if (escrow.state === 'disputed') {
        escrow = transitionContributorEscrow(escrow, 'release_pending', {
          at: this.now().toISOString(),
          expectedRevision: escrow.revision,
        });
        escrow = await this.store.saveEscrow(escrow);
      }
      if (escrow.state !== 'release_pending' || !escrow.reservationId) {
        throw new Error(`dispute release cannot resume from ${escrow.state}`);
      }
      const pullRequestUrl = bounty.activeClaim?.draftPullRequestUrl;
      if (!pullRequestUrl) throw new Error('release resolution pull request is missing');
      try {
        const operation = await this.policy.releaseEscrow(
          escrow.reservationId,
          releaseEvidence(
            bounty,
            pullRequestUrl,
            await this.reviewer.mergedEvidence(bounty, pullRequestUrl),
          ),
        );
        assertOperation(operation, 'escrow_release');
        if (operation.recipient !== escrow.recipientWallet) {
          throw new Error('dispute release recipient does not match the verified claimant wallet');
        }
        if (!operation.transactionSignature) throw new Error('dispute release has no transaction');
        escrow = transitionContributorEscrow(escrow, 'released', {
          at: this.now().toISOString(),
          expectedRevision: escrow.revision,
          transactionSignature: operation.transactionSignature,
        });
        escrow = await this.store.saveEscrow(escrow);
        return this.finalizeDisputeResolution(bounty, escrow, 'release');
      } catch (cause) {
        if (isRetryablePolicyError(cause)) return bounty;
        if (!isReleaseDeadlineError(cause) || !this.claimDeadlineReached(escrow)) throw cause;
        const pendingRefund = handoffRescueBountyDisputeReleaseToRefund(bounty, {
          at: this.now().toISOString(),
          expectedRevision: bounty.revision,
        });
        bounty = await this.store.updateBounty(pendingRefund, bounty.revision);
      }
    }

    if (escrow.state === 'disputed' || escrow.state === 'release_pending') {
      escrow = transitionContributorEscrow(escrow, 'refund_pending', {
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
        refundReasonCode: 'dispute_resolved',
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    if (escrow.state !== 'refund_pending' || !escrow.reservationId) {
      throw new Error(`dispute refund cannot resume from ${escrow.state}`);
    }
    let operation: PolicyOperation;
    try {
      operation = await this.policy.refundEscrow(escrow.reservationId, 'dispute_resolved');
    } catch (cause) {
      if (isRetryablePolicyError(cause)) return bounty;
      if (!this.claimDeadlineReached(escrow) && isRefundNotExpiredError(cause)) return bounty;
      throw cause;
    }
    assertOperation(operation, 'escrow_refund');
    if (!this.refundRecipient) throw new Error('escrow refund recipient is not configured');
    if (operation.recipient !== this.refundRecipient) {
      throw new Error('dispute refund recipient does not match the configured treasury');
    }
    if (!operation.transactionSignature) throw new Error('dispute refund has no transaction');
    escrow = transitionContributorEscrow(escrow, 'refunded', {
      at: this.now().toISOString(),
      expectedRevision: escrow.revision,
      transactionSignature: operation.transactionSignature,
    });
    escrow = await this.store.saveEscrow(escrow);
    return this.finalizeDisputeResolution(bounty, escrow, 'refund');
  }

  private async finalizeDisputeResolution(
    initial: RescueBounty,
    escrow: ContributorEscrow,
    decision: BountyDisputeDecision,
  ): Promise<RescueBounty> {
    const signature = decision === 'release' ? escrow.releaseSignature : escrow.refundSignature;
    if (!signature || escrow.state !== (decision === 'release' ? 'released' : 'refunded')) {
      throw new Error(`dispute ${decision} transaction evidence is incomplete`);
    }
    let bounty = await this.required(initial.id);
    if (bounty.state !== 'released' && bounty.state !== 'refunded') {
      const finalized = finalizeRescueBountyDisputeResolution(bounty, {
        decision,
        transactionSignature: signature,
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      bounty = await this.store.updateBounty(finalized, bounty.revision);
    }
    await this.publishDisputeResolution(bounty);
    return bounty;
  }

  private async publishDisputeResolution(bounty: RescueBounty): Promise<void> {
    const resolution = bounty.dispute?.resolution;
    if (!resolution?.resolvedAt || !resolution.transactionSignature) {
      throw new Error('resolved dispute evidence is incomplete');
    }
    const released = resolution.settlementDecision === 'release';
    const escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow?.amountAtomic) throw new Error('resolved dispute escrow principal is missing');
    await this.store.appendLedger({
      kind: released ? 'bounty_released' : 'bounty_returned',
      referenceId: bounty.id,
      asset: 'SOL',
      amountAtomic: escrow.amountAtomic,
      amountUsd: bounty.priceCents / 100,
      transaction: resolution.transactionSignature,
    });
    await this.store.appendActivity(
      'bounty.dispute_resolved',
      bounty.id,
      {
        disputeId: bounty.dispute?.id,
        requestedDecision: resolution.requestedDecision,
        settlementDecision: resolution.settlementDecision,
        evidenceHash: resolution.evidenceHash,
        transaction: resolution.transactionSignature,
        ...(bounty.activeClaim?.draftPullRequestUrl
          ? { pullRequestUrl: bounty.activeClaim.draftPullRequestUrl }
          : {}),
      },
      resolution.id,
    );
  }

  private async publishDisputeOpened(bounty: RescueBounty): Promise<void> {
    const dispute = bounty.dispute;
    if (!dispute) throw new Error('bounty dispute metadata is missing');
    await this.store.appendActivity(
      'bounty.disputed',
      bounty.id,
      {
        disputeId: dispute.id,
        claimantId: dispute.claimantId,
        reason: dispute.reason,
      },
      dispute.id,
    );
  }

  private async fund(bounty: RescueBounty): Promise<RescueBounty> {
    if (bounty.state === 'open') return bounty;
    if (!['draft', 'awaiting_funding', 'funding'].includes(bounty.state)) {
      throw new Error(`bounty funding cannot resume from ${bounty.state}`);
    }
    if (bounty.state !== 'funding') {
      const next = transitionRescueBounty(bounty, 'funding', {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      await this.store.updateBounty(next, bounty.revision);
    }
    let escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow) {
      const sourceJob = await this.store.job(bounty.sourceJobId);
      if (!sourceJob) throw new Error('source job is missing for bounty escrow');
      const acceptance = {
        bountyId: bounty.id,
        amountUsdCents: bounty.priceCents,
        expiresAt: bounty.offerExpiresAt,
        repository: bounty.repository,
        issueNumber: bounty.issueNumber,
        issueTitle: sourceJob.quote.issueTitle,
        issueBody: sourceJob.quote.issueBody,
        baseRef: sourceJob.quote.defaultBranch,
        baseSha: sourceJob.quote.baseSha,
        reviewPolicy: {
          version: 1 as const,
          model: this.reviewModel,
          maxFiles: sourceJob.quote.maxFiles,
        },
      };
      escrow = createContributorEscrow({
        id: randomUUID(),
        bountyId: bounty.id,
        repository: bounty.repository,
        issueNumber: bounty.issueNumber,
        issueTitle: sourceJob.quote.issueTitle,
        issueBody: sourceJob.quote.issueBody,
        baseRef: sourceJob.quote.defaultBranch,
        baseSha: sourceJob.quote.baseSha,
        reviewPolicy: acceptance.reviewPolicy,
        amountCents: bounty.priceCents,
        acceptanceHash: sha256({ kind: 'mizuki_contributor_escrow_acceptance', ...acceptance }),
        expiresAt: bounty.offerExpiresAt,
        at: this.now().toISOString(),
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    escrow = await this.reserveEscrow(escrow);
    if (
      escrow.state !== 'funded' ||
      !escrow.fundingSignature ||
      !escrow.reservationId ||
      !escrow.amountAtomic
    ) {
      throw new Error('bounty escrow reservation is incomplete');
    }
    await this.store.appendLedger({
      kind: 'bounty_reserved',
      referenceId: bounty.id,
      asset: 'SOL',
      amountAtomic: escrow.amountAtomic,
      amountUsd: bounty.priceCents / 100,
      transaction: escrow.fundingSignature,
    });
    const current = await this.required(bounty.id);
    if (current.state === 'open') return current;
    const opened = transitionRescueBounty(current, 'open', {
      at: this.now().toISOString(),
      expectedRevision: current.revision,
    });
    await this.store.updateBounty(opened, current.revision);
    await this.store.appendActivity('bounty.funded', bounty.id, {
      priceCents: bounty.priceCents,
      transaction: escrow.fundingSignature,
    });
    return opened;
  }

  private async reserveEscrow(initial: ContributorEscrow): Promise<ContributorEscrow> {
    let escrow = initial;
    if (['funded', 'bound', 'release_pending', 'released'].includes(escrow.state)) {
      return escrow;
    }
    if (escrow.state === 'requested') {
      escrow = transitionContributorEscrow(escrow, 'funding', {
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    if (escrow.state !== 'funding')
      throw new Error(`escrow funding cannot resume from ${escrow.state}`);
    const operation = await this.policy.reserveEscrow({
      bountyId: escrow.bountyId,
      repository: escrow.repository,
      issueNumber: escrow.issueNumber,
      baseRef: escrow.baseRef,
      baseSha: escrow.baseSha,
      amountUsdCents: escrow.amountCents,
      acceptanceHash: escrow.acceptanceHash,
      expiresAt: escrow.expiresAt,
      issueTitle: escrow.issueTitle,
      issueBody: escrow.issueBody,
      reviewPolicy: escrow.reviewPolicy,
    });
    assertOperation(operation, 'escrow_reserve');
    if (operation.amountUsdCents !== escrow.amountCents) {
      throw new Error('escrow funding amount does not match the accepted bounty');
    }
    if (operation.asset !== 'SOL' || !operation.amountAtomic || operation.amountAtomic === '0') {
      throw new Error('escrow funding principal is missing or uses the wrong asset');
    }
    if (!operation.transactionSignature) throw new Error('escrow funding has no transaction');
    escrow = transitionContributorEscrow(escrow, 'funded', {
      at: this.now().toISOString(),
      expectedRevision: escrow.revision,
      transactionSignature: operation.transactionSignature,
      reservationId: operation.id,
      amountAtomic: operation.amountAtomic,
    });
    return this.store.saveEscrow(escrow);
  }

  private async completeBinding(initial: ContributorEscrow): Promise<ContributorEscrow> {
    if (
      initial.state !== 'bind_pending' ||
      !initial.reservationId ||
      !initial.claimId ||
      !initial.claimSignature ||
      !initial.recipientWallet
    ) {
      throw new Error('escrow binding checkpoint is incomplete');
    }
    const operation = await this.policy.bindEscrow(
      initial.reservationId,
      initial.claimId,
      initial.claimSignature,
    );
    assertOperation(operation, 'escrow_bind');
    if (operation.recipient !== initial.recipientWallet) {
      throw new Error('escrow binding recipient does not match the signed wallet');
    }
    if (!operation.transactionSignature) throw new Error('escrow binding has no transaction');
    const bound = finalizeContributorEscrowBinding(initial, {
      bindOperationId: operation.id,
      transactionSignature: operation.transactionSignature,
      at: this.now().toISOString(),
      expectedRevision: initial.revision,
    });
    return this.store.saveEscrow(bound);
  }

  private async refundExpiredEscrow(initial: ContributorEscrow): Promise<ContributorEscrow> {
    let escrow = initial;
    if (escrow.state === 'refunded') return escrow;
    if (
      escrow.state === 'funded' ||
      escrow.state === 'bound' ||
      escrow.state === 'release_pending'
    ) {
      escrow = transitionContributorEscrow(escrow, 'refund_pending', {
        at: this.now().toISOString(),
        expectedRevision: escrow.revision,
        refundReasonCode: 'expired',
      });
      escrow = await this.store.saveEscrow(escrow);
    }
    if (escrow.state !== 'refund_pending' || !escrow.reservationId) {
      throw new Error(`escrow refund cannot resume from ${escrow.state}`);
    }
    const operation = await this.policy.refundEscrow(
      escrow.reservationId,
      escrow.refundReasonCode ?? 'expired',
    );
    assertOperation(operation, 'escrow_refund');
    if (!this.refundRecipient) throw new Error('escrow refund recipient is not configured');
    if (operation.recipient !== this.refundRecipient) {
      throw new Error('escrow refund recipient does not match the configured treasury');
    }
    if (!operation.transactionSignature) throw new Error('escrow refund has no transaction');
    escrow = transitionContributorEscrow(escrow, 'refunded', {
      at: this.now().toISOString(),
      expectedRevision: escrow.revision,
      transactionSignature: operation.transactionSignature,
    });
    escrow = await this.store.saveEscrow(escrow);
    if (!escrow.amountAtomic) throw new Error('refunded escrow principal is missing');
    await this.store.appendLedger({
      kind: 'bounty_returned',
      referenceId: escrow.bountyId,
      asset: 'SOL',
      amountAtomic: escrow.amountAtomic,
      amountUsd: escrow.amountCents / 100,
      transaction: escrow.refundSignature,
    });
    return escrow;
  }

  private async settleExpiredClaim(bounty: RescueBounty): Promise<RescueBounty> {
    const claim = bounty.activeClaim;
    if (bounty.state !== 'claim_refund_pending' || claim?.state !== 'expired') {
      throw new Error('bounty claim refund is not pending');
    }
    const matching = (await this.store.escrowsByBounty(bounty.id)).filter(
      (escrow) => escrow.claimId === claim.id,
    );
    if (matching.length === 0) throw new Error('expired claim escrow is missing');
    for (const escrow of matching) {
      if (escrow.state !== 'refunded') await this.refundExpiredEscrow(escrow);
    }
    const settled = (await this.store.escrowsByBounty(bounty.id)).filter(
      (escrow) => escrow.claimId === claim.id,
    );
    if (settled.some((escrow) => escrow.state !== 'refunded')) {
      throw new Error('expired claim escrow refund is incomplete');
    }
    const current = await this.required(bounty.id);
    const finalized = finalizeRescueBountyClaimRefund(current, {
      at: this.now().toISOString(),
      expectedRevision: current.revision,
    });
    const closed = await this.store.updateBounty(finalized, current.revision);
    const job = await this.store.job(closed.sourceJobId);
    if (!job) throw new Error('source job is missing for replacement bounty');
    await this.createGeneration(job, closed.generation + 1, closed.id);
    return closed;
  }

  private async settleExpiredOffer(bounty: RescueBounty): Promise<RescueBounty> {
    if (bounty.state !== 'offer_refund_pending' || bounty.activeClaim) {
      throw new Error('bounty offer refund is not pending');
    }
    const escrow = await this.store.escrowByBounty(bounty.id);
    if (!escrow) throw new Error('expired bounty offer escrow is missing');
    const refunded = await this.refundExpiredEscrow(escrow);
    if (refunded.state !== 'refunded') throw new Error('bounty offer refund is incomplete');
    const current = await this.required(bounty.id);
    const expired = finalizeRescueBountyOfferRefund(current, {
      at: this.now().toISOString(),
      expectedRevision: current.revision,
    });
    const closed = await this.store.updateBounty(expired, current.revision);
    await this.store.appendActivity('bounty.expired', bounty.id, {
      reason: 'offer_expired',
      transaction: refunded.refundSignature,
    });
    return closed;
  }

  private async settleExpiredRelease(
    initial: RescueBounty,
    pullRequestUrl: string,
    initialEscrow?: ContributorEscrow,
  ): Promise<RescueBounty> {
    let bounty = initial;
    let escrow = initialEscrow ?? (await this.store.escrowByBounty(bounty.id));
    if (!escrow?.reservationId || !escrow.recipientWallet) {
      throw new Error('expired release escrow is incomplete');
    }
    if (!this.claimDeadlineReached(escrow)) {
      throw new Error('contributor release deadline has not elapsed');
    }
    if (bounty.state === 'accepted') {
      const pending = expireAcceptedRescueBountyRelease(bounty, {
        at: this.now().toISOString(),
        expectedRevision: bounty.revision,
      });
      bounty = await this.store.updateBounty(pending, bounty.revision);
      await this.store.appendActivity('bounty.expired', bounty.id, {
        reason: 'release_deadline_elapsed',
      });
    }
    if (bounty.state !== 'release_refund_pending') {
      if (bounty.state === 'released' || bounty.state === 'refunded') return bounty;
      throw new Error(`expired release cannot resume from ${bounty.state}`);
    }

    if (escrow.state === 'release_pending' || escrow.state === 'refund_pending') {
      try {
        const release = await this.policy.releaseEscrow(
          escrow.reservationId,
          releaseEvidence(
            bounty,
            pullRequestUrl,
            await this.reviewer.mergedEvidence(bounty, pullRequestUrl),
          ),
        );
        assertOperation(release, 'escrow_release');
        if (release.recipient !== escrow.recipientWallet || !release.transactionSignature) {
          throw new Error('reconciled escrow release evidence does not match the claimant');
        }
        escrow = transitionContributorEscrow(escrow, 'released', {
          at: this.now().toISOString(),
          expectedRevision: escrow.revision,
          transactionSignature: release.transactionSignature,
        });
        escrow = await this.store.saveEscrow(escrow);
      } catch {
        // At or after the immutable deadline, the on-chain guard makes refund and release exclusive.
      }
    }
    if (escrow.state === 'released') {
      return this.finalizeReleasedBounty(bounty, escrow, pullRequestUrl);
    }

    escrow = await this.refundExpiredEscrow(escrow);
    if (escrow.state !== 'refunded') throw new Error('expired release escrow refund is incomplete');
    const current = await this.required(bounty.id);
    const finalized = finalizeExpiredReleaseRefund(current, {
      at: this.now().toISOString(),
      expectedRevision: current.revision,
    });
    const closed = await this.store.updateBounty(finalized, current.revision);
    await this.store.appendActivity('bounty.expired', closed.id, {
      reason: 'release_refunded',
      transaction: escrow.refundSignature,
    });
    return closed;
  }

  private async finalizeReleasedBounty(
    initial: RescueBounty,
    escrow: ContributorEscrow,
    pullRequestUrl: string,
  ): Promise<RescueBounty> {
    if (escrow.state !== 'released' || !escrow.releaseSignature || !escrow.amountAtomic) {
      throw new Error('contributor escrow release is incomplete');
    }
    const bounty = await this.required(initial.id);
    if (bounty.state === 'released') return bounty;
    const released = transitionRescueBounty(bounty, 'released', {
      at: this.now().toISOString(),
      expectedRevision: bounty.revision,
    });
    await this.store.updateBounty(released, bounty.revision);
    await this.store.appendLedger({
      kind: 'bounty_released',
      referenceId: bounty.id,
      asset: 'SOL',
      amountAtomic: escrow.amountAtomic,
      amountUsd: bounty.priceCents / 100,
      transaction: escrow.releaseSignature,
    });
    await this.store.appendActivity('bounty.released', bounty.id, {
      pullRequestUrl,
      transaction: escrow.releaseSignature,
    });
    return released;
  }

  private claimDeadlineReached(escrow: ContributorEscrow): boolean {
    return Boolean(
      escrow.claimExpiresAt && Date.parse(escrow.claimExpiresAt) <= this.now().getTime(),
    );
  }

  private async recoverBoundClaim(
    bounty: RescueBounty,
    escrow: ContributorEscrow,
  ): Promise<RescueBounty> {
    if (
      escrow.state !== 'bound' ||
      !escrow.claimId ||
      !escrow.claimantId ||
      !escrow.recipientWallet ||
      !escrow.claimExpiresAt
    ) {
      throw new Error('bound escrow is missing claimant evidence');
    }
    const expiresAt = Date.parse(escrow.claimExpiresAt);
    const at = new Date(Math.min(this.now().getTime(), expiresAt - 1)).toISOString();
    const claimed = claimRescueBounty(bounty, {
      claimId: escrow.claimId,
      claimantId: escrow.claimantId,
      walletAddress: escrow.recipientWallet,
      leaseExpiresAt: escrow.claimExpiresAt,
      at,
      expectedRevision: bounty.revision,
    });
    let recovered = await this.store.updateBounty(claimed, bounty.revision);
    if (expiresAt <= this.now().getTime()) {
      const pending = expireRescueBountyClaim(recovered, {
        at: this.now().toISOString(),
        expectedRevision: recovered.revision,
      });
      recovered = await this.store.updateBounty(pending, recovered.revision);
      await this.settleExpiredClaim(recovered);
    }
    return recovered;
  }

  private async validate(bounty: RescueBounty, recovering = false): Promise<RescueBounty> {
    const pullRequestUrl = bounty.activeClaim?.draftPullRequestUrl;
    if (!pullRequestUrl) throw new Error('bounty has no submitted pull request');
    if (bounty.validationReceipt || bounty.validationAttempt?.status === 'completed') return bounty;
    if (bounty.validationAttempt?.status === 'failed') return bounty;

    let validating = bounty;
    if (validating.state !== 'validating') {
      const startedAt = this.now().toISOString();
      const transitioned = transitionRescueBounty(validating, 'validating', {
        at: startedAt,
        expectedRevision: validating.revision,
      });
      const id = randomUUID();
      validating = {
        ...transitioned,
        validationAttempt: {
          id,
          requestKey: id,
          pullRequestUrl,
          status: 'reserved',
          maxCostMicrounits: String(this.reviewMaxCostMicrounits),
          startedAt,
          updatedAt: startedAt,
        },
      };
      validating = await this.store.updateBounty(validating, bounty.revision);
    }

    const attempt = validating.validationAttempt;
    if (!attempt || attempt.pullRequestUrl !== pullRequestUrl) return validating;
    if (attempt.status === 'submitted' || attempt.status === 'received') {
      if (!recovering || !submittedReviewIsStale(attempt, this.now())) return validating;
      return this.failValidation(
        validating,
        attempt.id,
        new Error(
          'paid review outcome is indeterminate after recovery; the provider request will not be retried',
        ),
        'indeterminate_after_recovery',
      );
    }
    if (attempt.status !== 'reserved') return validating;
    const maxCostMicrounits = durableReviewMaxCost(attempt.maxCostMicrounits);
    const preflight = await this.reviewer.preflight(validating, pullRequestUrl, {
      id: attempt.requestKey,
      maxCostMicrounits,
    });

    if (preflight.kind === 'rejected') {
      return this.completeValidation(validating, attempt, preflight.result);
    }

    await this.store.appendLedger({
      kind: 'operating_cost',
      referenceId: `bounty-review:${attempt.id}`,
      asset: 'USD',
      amountAtomic: '0',
      amountUsd: maxCostMicrounits / 1_000_000,
    });
    validating = await this.markValidationSubmitted(validating, attempt);

    let review: ContributorPatchReviewResult;
    try {
      review = await this.reviewer.review(preflight, async (accounting) => {
        validating = await this.markValidationReceived(validating, attempt.id, accounting);
      });
    } catch (providerError) {
      try {
        await this.failValidation(
          validating,
          attempt.id,
          providerError,
          'provider_error',
          providerError instanceof ContributorPatchReviewError
            ? providerError.accounting
            : undefined,
        );
      } catch (checkpointError) {
        throw new AggregateError(
          [providerError, checkpointError],
          'bounty review provider and terminal checkpoint both failed',
        );
      }
      throw providerError;
    }

    return this.completeValidation(validating, validating.validationAttempt ?? attempt, review);
  }

  private async completeValidation(
    validating: RescueBounty,
    attempt: BountyValidationAttempt,
    review: ContributorPatchReviewResult,
  ): Promise<RescueBounty> {
    const reviewed = transitionRescueBounty(validating, 'pr_submitted', {
      at: this.now().toISOString(),
      expectedRevision: validating.revision,
    });
    const completedBounty: RescueBounty = {
      ...reviewed,
      validationAttempt: {
        ...attempt,
        status: 'completed',
        updatedAt: reviewed.updatedAt,
      },
      validationReceipt: {
        id: randomUUID(),
        approved: review.approved,
        reason: review.reason,
        reviewedAt: this.now().toISOString(),
        headSha: review.headSha,
        baseSha: review.baseSha,
        baseRef: review.baseRef,
        diffHash: review.diffHash,
        ...(review.providerReceipt ? { provider: review.providerReceipt } : {}),
        ...(review.inputTokens === undefined ? {} : { inputTokens: review.inputTokens }),
        ...(review.outputTokens === undefined ? {} : { outputTokens: review.outputTokens }),
      },
    };
    await this.store.updateBounty(completedBounty, validating.revision);
    return completedBounty;
  }

  private async markValidationSubmitted(
    bounty: RescueBounty,
    attempt: BountyValidationAttempt,
  ): Promise<RescueBounty> {
    const updatedAt = this.now().toISOString();
    const submitted: RescueBounty = {
      ...bounty,
      validationAttempt: { ...attempt, status: 'submitted', updatedAt },
      updatedAt,
      revision: bounty.revision + 1,
    };
    return this.store.updateBounty(submitted, bounty.revision);
  }

  private async markValidationReceived(
    bounty: RescueBounty,
    attemptId: string,
    accounting: ContributorPatchReviewAccounting,
  ): Promise<RescueBounty> {
    const current = await this.required(bounty.id);
    const attempt = current.validationAttempt;
    if (current.state !== 'validating' || attempt?.id !== attemptId) {
      throw new Error('bounty review attempt changed before receipt checkpoint');
    }
    if (attempt.status === 'received') return current;
    if (attempt.status !== 'submitted') {
      throw new Error('bounty review attempt is not awaiting a provider receipt');
    }
    const updatedAt = this.now().toISOString();
    const received: RescueBounty = {
      ...current,
      validationAttempt: {
        ...attempt,
        status: 'received',
        updatedAt,
        provider: accounting.providerReceipt,
        ...(accounting.inputTokens === undefined ? {} : { inputTokens: accounting.inputTokens }),
        ...(accounting.outputTokens === undefined ? {} : { outputTokens: accounting.outputTokens }),
      },
      updatedAt,
      revision: current.revision + 1,
    };
    return this.store.updateBounty(received, current.revision);
  }

  private async failValidation(
    bounty: RescueBounty,
    attemptId: string,
    error: unknown,
    failureKind: NonNullable<BountyValidationAttempt['failureKind']>,
    accounting?: ContributorPatchReviewAccounting,
  ): Promise<RescueBounty> {
    const current = await this.required(bounty.id);
    if (
      current.state !== 'validating' ||
      current.validationAttempt?.id !== attemptId ||
      !['submitted', 'received'].includes(current.validationAttempt.status)
    ) {
      return current;
    }
    const failed = transitionRescueBounty(current, 'pr_submitted', {
      at: this.now().toISOString(),
      expectedRevision: current.revision,
    });
    const value: RescueBounty = {
      ...failed,
      validationAttempt: {
        ...current.validationAttempt,
        status: 'failed',
        updatedAt: failed.updatedAt,
        failureKind,
        error: (error instanceof Error ? error.message : String(error)).slice(0, 2_000),
        ...(accounting?.providerReceipt ? { provider: accounting.providerReceipt } : {}),
        ...(accounting?.inputTokens === undefined ? {} : { inputTokens: accounting.inputTokens }),
        ...(accounting?.outputTokens === undefined
          ? {}
          : { outputTokens: accounting.outputTokens }),
      },
    };
    return this.store.updateBounty(value, current.revision);
  }

  private async required(id: string): Promise<RescueBounty> {
    const bounty = await this.store.bounty(id);
    if (!bounty) throw new Error(`unknown bounty: ${id}`);
    return bounty;
  }
}

function durableReviewMaxCost(value: unknown): number {
  if (typeof value !== 'string' || value.length > 7 || !/^[1-9]\d*$/.test(value)) {
    throw new Error('bounty review attempt has an invalid durable cost reservation');
  }
  const parsed = Number(value);
  if (
    !Number.isSafeInteger(parsed) ||
    parsed <= 0 ||
    parsed > MAX_BOUNTY_REVIEW_COST_MICROUNITS ||
    String(parsed) !== value
  ) {
    throw new Error('bounty review attempt has an invalid durable cost reservation');
  }
  return parsed;
}

function submittedReviewIsStale(attempt: BountyValidationAttempt, now: Date): boolean {
  const submittedAt = Date.parse(attempt.updatedAt);
  return !Number.isFinite(submittedAt) || now.getTime() - submittedAt >= SUBMITTED_REVIEW_STALE_MS;
}

function sha256(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

function assertOperation(operation: PolicyOperation, kind: PolicyOperation['kind']): void {
  if (operation.kind !== kind || operation.status !== 'finalized') {
    throw new Error(`policy signer returned an invalid ${kind} operation`);
  }
}

function pullRequestNumber(value: string, repository: string): number {
  const url = new URL(value);
  const match = new RegExp(`^/${escapeRegExp(repository)}/pull/([1-9][0-9]*)$`, 'i').exec(
    url.pathname,
  );
  if (!match?.[1]) throw new Error('pull request URL does not match the bounty repository');
  return Number(match[1]);
}

function releaseEvidence(
  bounty: RescueBounty,
  pullRequestUrl: string,
  merge: {
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    mergedAt: string;
    mergeCommitSha: string;
  },
) {
  const receipt = bounty.validationReceipt;
  assertReleaseEvidenceAvailable(bounty, pullRequestUrl);
  assertReviewedMerge(bounty, merge);
  if (!receipt?.provider) {
    throw new Error('bounty review evidence is incomplete');
  }
  return {
    repository: bounty.repository,
    issueNumber: bounty.issueNumber,
    pullRequestNumber: pullRequestNumber(pullRequestUrl, bounty.repository),
    mergeCommitSha: merge.mergeCommitSha,
    reviewedHeadSha: receipt.headSha,
    reviewedBaseSha: receipt.baseSha,
    reviewedBaseRef: receipt.baseRef,
    reviewedDiffHash: receipt.diffHash,
    reviewReceiptId: receipt.id,
    reviewReceiptHash: sha256({ version: 1, ...receipt }),
    reviewModel: receipt.provider.model,
    reviewRoute: receipt.provider.route,
    reviewedAt: receipt.reviewedAt,
  };
}

function assertReleaseEvidenceAvailable(bounty: RescueBounty, pullRequestUrl: string): void {
  const receipt = bounty.validationReceipt;
  pullRequestNumber(pullRequestUrl, bounty.repository);
  if (
    !receipt?.approved ||
    !receipt.headSha ||
    !receipt.baseSha ||
    !receipt.baseRef ||
    !receipt.diffHash ||
    !receipt.provider
  ) {
    throw new Error('bounty review evidence is incomplete');
  }
}

function assertReviewedMerge(
  bounty: RescueBounty,
  evidence: { headSha: string; baseSha: string; baseRef: string; diffHash: string },
): void {
  const receipt = bounty.validationReceipt;
  if (
    !receipt?.approved ||
    !receipt.headSha ||
    !receipt.baseSha ||
    !receipt.baseRef ||
    !receipt.diffHash
  ) {
    throw new Error('bounty review evidence is incomplete');
  }
  if (
    evidence.headSha !== receipt.headSha ||
    evidence.baseSha !== receipt.baseSha ||
    evidence.baseRef !== receipt.baseRef ||
    evidence.diffHash !== receipt.diffHash
  ) {
    throw new Error('merged pull request does not match the independently reviewed revision');
  }
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function isReleaseDeadlineError(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : String(cause);
  return /escrow claim has expired|release deadline elapsed|release_deadline_elapsed|pull request merged after .*claim expiry|github_merge_after_expiry/i.test(
    message,
  );
}

function isRefundNotExpiredError(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : String(cause);
  return /escrow cannot be refunded before expiry|escrow_not_expired/i.test(message);
}

function isRetryablePolicyError(cause: unknown): boolean {
  if (cause instanceof PendingPolicyOperationError) return true;
  if (cause instanceof PolicyRequestError) return cause.retryable;
  if (cause instanceof TypeError) return true;
  return (
    cause instanceof DOMException && (cause.name === 'AbortError' || cause.name === 'TimeoutError')
  );
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
    .join(',')}}`;
}
