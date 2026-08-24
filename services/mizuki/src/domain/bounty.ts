import {
  DomainRuleError,
  addHours,
  assertExpectedRevision,
  assertNonEmpty,
  assertNotBefore,
  assertTransition,
  assertUsdCents,
  timestampMs,
  type TransitionTable,
} from './state-machine.js';
import { fingerprintPayload, normalizeIdempotencyKey } from './idempotency.js';

export const CLAIM_LEASE_HOURS = 48;
export const BOUNTY_OFFER_HOURS = 7 * 24;
export const MIN_RESCUE_BOUNTY_CENTS = 1_000;
export const MAX_RESCUE_BOUNTY_CENTS = 2_500;

export type RescueBountyState =
  | 'draft'
  | 'awaiting_funding'
  | 'funding'
  | 'open'
  | 'claimed'
  | 'pr_submitted'
  | 'validating'
  | 'claim_refund_pending'
  | 'offer_refund_pending'
  | 'release_refund_pending'
  | 'accepted'
  | 'released'
  | 'expired'
  | 'rejected'
  | 'disputed'
  | 'refunded';

export type BountyClaimState =
  | 'active'
  | 'draft_submitted'
  | 'validating'
  | 'accepted'
  | 'released'
  | 'expired'
  | 'rejected'
  | 'disputed'
  | 'refunded';

export type BountyClaim = {
  id: string;
  claimantId: string;
  walletAddress: string;
  state: BountyClaimState;
  claimedAt: string;
  leaseExpiresAt: string;
  draftPullRequestUrl?: string;
  draftSubmittedAt?: string;
  closedAt?: string;
};

export type BountyDisputeDecision = 'release' | 'refund';

export type BountyDisputeEvidence = {
  summary: string;
  references: readonly string[];
};

export type BountyDispute = {
  id: string;
  claimantId: string;
  reason: string;
  state: 'open' | 'release_pending' | 'refund_pending' | 'released' | 'refunded';
  openedAt: string;
  resolution?: {
    id: string;
    idempotencyKey: string;
    requestedDecision: BountyDisputeDecision;
    settlementDecision: BountyDisputeDecision;
    evidence: BountyDisputeEvidence;
    evidenceHash: string;
    decidedAt: string;
    resolvedAt?: string;
    transactionSignature?: string;
    fallbackReason?: 'release_deadline_elapsed';
  };
};

export type BountyValidationAttempt = {
  id: string;
  requestKey: string;
  pullRequestUrl: string;
  status: 'reserved' | 'submitted' | 'completed' | 'failed';
  maxCostMicrounits: string;
  startedAt: string;
  updatedAt: string;
  failureKind?: 'provider_error' | 'indeterminate_after_recovery';
  error?: string;
};

export type RescueBounty = {
  id: string;
  sourceJobId: string;
  failureReceiptId: string;
  repository: string;
  issueNumber: number;
  issueUrl: string;
  priceCents: number;
  generation: number;
  predecessorBountyId?: string;
  offerExpiresAt: string;
  state: RescueBountyState;
  activeClaim?: BountyClaim;
  validationReceipt?: {
    id: string;
    approved: boolean;
    reason: string;
    reviewedAt: string;
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    provider?: {
      model: string;
      route: 'marketplace';
      providerId?: string;
      requestId?: string;
      costMicrounits?: string;
    };
  };
  validationAttempt?: BountyValidationAttempt;
  dispute?: BountyDispute;
  claimHistory: readonly BountyClaim[];
  createdAt: string;
  updatedAt: string;
  revision: number;
};

export type BountyCommand = {
  at: string;
  expectedRevision: number;
};

const transitions: TransitionTable<RescueBountyState> = {
  draft: ['awaiting_funding', 'funding', 'expired'],
  awaiting_funding: ['funding', 'expired'],
  funding: ['open', 'awaiting_funding', 'refunded'],
  open: ['claimed', 'offer_refund_pending', 'refunded'],
  claimed: ['open', 'pr_submitted', 'disputed', 'refunded'],
  pr_submitted: ['open', 'validating', 'disputed', 'refunded'],
  validating: ['pr_submitted', 'accepted', 'rejected', 'disputed', 'refunded'],
  claim_refund_pending: ['refunded'],
  offer_refund_pending: ['expired'],
  accepted: ['released', 'release_refund_pending'],
  release_refund_pending: ['released', 'refunded'],
  released: [],
  expired: [],
  rejected: [],
  disputed: ['released', 'refunded', 'rejected'],
  refunded: [],
};

const claimStateForBounty: Partial<Record<RescueBountyState, BountyClaimState>> = {
  claimed: 'active',
  pr_submitted: 'draft_submitted',
  validating: 'validating',
  accepted: 'accepted',
  released: 'released',
  rejected: 'rejected',
  disputed: 'disputed',
  refunded: 'refunded',
};

export function calculateRescueBountyPriceCents(jobPriceCents: number): number {
  assertUsdCents(jobPriceCents, 'job price');
  if (jobPriceCents === 0) {
    throw new DomainRuleError('INVALID_JOB_PRICE', 'Job price must be greater than zero');
  }
  const doubled = jobPriceCents * 2;
  if (!Number.isSafeInteger(doubled)) {
    throw new DomainRuleError('INVALID_JOB_PRICE', 'Job price is too large');
  }
  return Math.min(MAX_RESCUE_BOUNTY_CENTS, Math.max(MIN_RESCUE_BOUNTY_CENTS, doubled));
}

export function createRescueBounty(input: {
  id: string;
  sourceJobId: string;
  failureReceiptId: string;
  repository: string;
  issueNumber: number;
  issueUrl: string;
  jobPriceCents: number;
  generation?: number;
  predecessorBountyId?: string;
  at: string;
}): RescueBounty {
  if (!Number.isSafeInteger(input.issueNumber) || input.issueNumber <= 0) {
    throw new DomainRuleError('INVALID_ISSUE', 'Issue number must be a positive integer');
  }
  validateIssueUrl(input.issueUrl, input.repository, input.issueNumber);
  const createdAt = new Date(timestampMs(input.at, 'created at')).toISOString();
  const generation = input.generation ?? 0;
  if (!Number.isSafeInteger(generation) || generation < 0) {
    throw new DomainRuleError(
      'INVALID_BOUNTY_GENERATION',
      'Bounty generation must be non-negative',
    );
  }

  return {
    id: assertNonEmpty(input.id, 'bounty id'),
    sourceJobId: assertNonEmpty(input.sourceJobId, 'source job id'),
    failureReceiptId: assertNonEmpty(input.failureReceiptId, 'failure receipt id'),
    repository: normalizeRepository(input.repository),
    issueNumber: input.issueNumber,
    issueUrl: input.issueUrl,
    priceCents: calculateRescueBountyPriceCents(input.jobPriceCents),
    generation,
    ...(input.predecessorBountyId
      ? { predecessorBountyId: assertNonEmpty(input.predecessorBountyId, 'predecessor bounty id') }
      : {}),
    offerExpiresAt: addHours(createdAt, BOUNTY_OFFER_HOURS),
    state: 'draft',
    claimHistory: [],
    createdAt,
    updatedAt: createdAt,
    revision: 0,
  };
}

export function transitionRescueBounty(
  bounty: RescueBounty,
  to: RescueBountyState,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'transition time');
  assertTransition(transitions, bounty.state, to, 'Rescue bounty');

  if (
    to === 'claimed' ||
    to === 'disputed' ||
    (to === 'pr_submitted' && bounty.state === 'claimed') ||
    (to === 'open' && bounty.activeClaim)
  ) {
    throw new DomainRuleError(
      'BOUNTY_COMMAND_REQUIRED',
      'Claim lifecycle transitions require their dedicated command',
    );
  }

  const claimState = claimStateForBounty[to];
  const claimRequired =
    to === 'validating' || to === 'accepted' || to === 'released' || to === 'rejected';
  if (claimRequired && !bounty.activeClaim) {
    throw new DomainRuleError('MISSING_ACTIVE_CLAIM', `${to} requires an active claim`);
  }

  const updatedAt = new Date(timestampMs(command.at)).toISOString();
  const activeClaim =
    claimState && bounty.activeClaim
      ? {
          ...bounty.activeClaim,
          state: claimState,
          ...(claimState === 'released' || claimState === 'rejected' || claimState === 'refunded'
            ? { closedAt: updatedAt }
            : {}),
        }
      : bounty.activeClaim;

  return {
    ...bounty,
    state: to,
    activeClaim,
    updatedAt,
    revision: bounty.revision + 1,
  };
}

export function openRescueBountyDispute(
  bounty: RescueBounty,
  input: BountyCommand & {
    disputeId: string;
    claimantId: string;
    reason: string;
  },
): RescueBounty {
  assertExpectedRevision(bounty.revision, input.expectedRevision);
  assertNotBefore(input.at, bounty.updatedAt, 'dispute open time');
  assertTransition(transitions, bounty.state, 'disputed', 'Rescue bounty');
  const claim = requireActiveClaim(bounty);
  const claimantId = assertNonEmpty(input.claimantId, 'dispute claimant id');
  if (claim.claimantId !== claimantId) {
    throw new DomainRuleError('DISPUTE_CLAIMANT_MISMATCH', 'Only the active claimant may dispute');
  }
  const reason = input.reason.trim();
  if (reason.length < 10 || reason.length > 2_000) {
    throw new DomainRuleError(
      'INVALID_DISPUTE_REASON',
      'Dispute reason must contain 10-2000 characters',
    );
  }
  const openedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...bounty,
    state: 'disputed',
    activeClaim: { ...claim, state: 'disputed' },
    dispute: {
      id: assertNonEmpty(input.disputeId, 'dispute id'),
      claimantId,
      reason,
      state: 'open',
      openedAt,
    },
    updatedAt: openedAt,
    revision: bounty.revision + 1,
  };
}

export function beginRescueBountyDisputeResolution(
  bounty: RescueBounty,
  input: BountyCommand & {
    disputeId: string;
    resolutionId: string;
    idempotencyKey: string;
    decision: BountyDisputeDecision;
    evidence: BountyDisputeEvidence;
  },
): RescueBounty {
  assertExpectedRevision(bounty.revision, input.expectedRevision);
  assertNotBefore(input.at, bounty.updatedAt, 'dispute decision time');
  if (bounty.state !== 'disputed' || !bounty.dispute) {
    throw new DomainRuleError('DISPUTE_NOT_OPEN', 'Bounty does not have an open dispute');
  }
  if (bounty.dispute.id !== input.disputeId) {
    throw new DomainRuleError('DISPUTE_MISMATCH', 'Dispute id does not match the bounty');
  }
  if (bounty.dispute.state !== 'open' || bounty.dispute.resolution) {
    throw new DomainRuleError('DISPUTE_ALREADY_DECIDED', 'Dispute already has a decision');
  }
  if (input.decision !== 'release' && input.decision !== 'refund') {
    throw new DomainRuleError('INVALID_DISPUTE_DECISION', 'Dispute decision is invalid');
  }
  const evidence = normalizeBountyDisputeEvidence(input.evidence);
  const evidenceHash = fingerprintBountyDisputeEvidence(evidence);
  const decidedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...bounty,
    dispute: {
      ...bounty.dispute,
      state: input.decision === 'release' ? 'release_pending' : 'refund_pending',
      resolution: {
        id: assertNonEmpty(input.resolutionId, 'dispute resolution id'),
        idempotencyKey: normalizeIdempotencyKey(input.idempotencyKey),
        requestedDecision: input.decision,
        settlementDecision: input.decision,
        evidence,
        evidenceHash,
        decidedAt,
      },
    },
    updatedAt: decidedAt,
    revision: bounty.revision + 1,
  };
}

export function handoffRescueBountyDisputeReleaseToRefund(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'dispute release handoff time');
  const dispute = bounty.dispute;
  if (
    bounty.state !== 'disputed' ||
    dispute?.state !== 'release_pending' ||
    dispute.resolution?.settlementDecision !== 'release'
  ) {
    throw new DomainRuleError(
      'DISPUTE_RELEASE_NOT_PENDING',
      'Dispute does not have a pending release',
    );
  }
  const claim = requireActiveClaim(bounty);
  if (timestampMs(command.at) < timestampMs(claim.leaseExpiresAt)) {
    throw new DomainRuleError('CLAIM_STILL_ACTIVE', 'Claim lease has not expired');
  }
  const updatedAt = new Date(timestampMs(command.at)).toISOString();
  return {
    ...bounty,
    dispute: {
      ...dispute,
      state: 'refund_pending',
      resolution: {
        ...dispute.resolution,
        settlementDecision: 'refund',
        fallbackReason: 'release_deadline_elapsed',
      },
    },
    updatedAt,
    revision: bounty.revision + 1,
  };
}

export function finalizeRescueBountyDisputeResolution(
  bounty: RescueBounty,
  input: BountyCommand & {
    decision: BountyDisputeDecision;
    transactionSignature: string;
  },
): RescueBounty {
  assertExpectedRevision(bounty.revision, input.expectedRevision);
  assertNotBefore(input.at, bounty.updatedAt, 'dispute resolution time');
  const dispute = bounty.dispute;
  const resolution = dispute?.resolution;
  if (
    bounty.state !== 'disputed' ||
    !dispute ||
    !resolution ||
    dispute.state !== `${input.decision}_pending` ||
    resolution.settlementDecision !== input.decision
  ) {
    throw new DomainRuleError(
      'DISPUTE_RESOLUTION_NOT_PENDING',
      `Dispute does not have a pending ${input.decision}`,
    );
  }
  const claim = requireActiveClaim(bounty);
  const resolvedAt = new Date(timestampMs(input.at)).toISOString();
  const transactionSignature = assertNonEmpty(
    input.transactionSignature,
    'dispute resolution transaction',
  );
  return {
    ...bounty,
    state: input.decision === 'release' ? 'released' : 'refunded',
    activeClaim: {
      ...claim,
      state: input.decision === 'release' ? 'released' : 'refunded',
      closedAt: resolvedAt,
    },
    dispute: {
      ...dispute,
      state: input.decision === 'release' ? 'released' : 'refunded',
      resolution: {
        ...resolution,
        resolvedAt,
        transactionSignature,
      },
    },
    updatedAt: resolvedAt,
    revision: bounty.revision + 1,
  };
}

export function claimRescueBounty(
  bounty: RescueBounty,
  input: BountyCommand & {
    claimId: string;
    claimantId: string;
    walletAddress: string;
    leaseExpiresAt?: string;
  },
): RescueBounty {
  assertExpectedRevision(bounty.revision, input.expectedRevision);
  assertNotBefore(input.at, bounty.updatedAt, 'claim time');
  assertTransition(transitions, bounty.state, 'claimed', 'Rescue bounty');
  if (bounty.activeClaim) {
    throw new DomainRuleError('BOUNTY_ALREADY_CLAIMED', 'Bounty already has an active claim');
  }

  const claimedAt = new Date(timestampMs(input.at)).toISOString();
  const leaseExpiresAt = input.leaseExpiresAt
    ? new Date(timestampMs(input.leaseExpiresAt, 'claim lease expiry')).toISOString()
    : addHours(claimedAt, CLAIM_LEASE_HOURS);
  if (timestampMs(leaseExpiresAt) <= timestampMs(claimedAt)) {
    throw new DomainRuleError('INVALID_CLAIM_EXPIRY', 'Claim lease expiry must follow claim time');
  }
  const activeClaim: BountyClaim = {
    id: assertNonEmpty(input.claimId, 'claim id'),
    claimantId: assertNonEmpty(input.claimantId, 'claimant id'),
    walletAddress: assertNonEmpty(input.walletAddress, 'wallet address'),
    state: 'active',
    claimedAt,
    leaseExpiresAt,
  };

  return {
    ...bounty,
    state: 'claimed',
    activeClaim,
    updatedAt: claimedAt,
    revision: bounty.revision + 1,
  };
}

export function submitDraftPullRequest(
  bounty: RescueBounty,
  input: BountyCommand & { pullRequestUrl: string },
): RescueBounty {
  assertExpectedRevision(bounty.revision, input.expectedRevision);
  assertNotBefore(input.at, bounty.updatedAt, 'draft submission time');
  assertTransition(transitions, bounty.state, 'pr_submitted', 'Rescue bounty');
  const claim = requireActiveClaim(bounty);
  if (timestampMs(input.at) >= timestampMs(claim.leaseExpiresAt)) {
    throw new DomainRuleError('CLAIM_EXPIRED', 'Claim lease has expired');
  }
  validatePullRequestUrl(input.pullRequestUrl, bounty.repository);

  const submittedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...bounty,
    state: 'pr_submitted',
    activeClaim: {
      ...claim,
      state: 'draft_submitted',
      draftPullRequestUrl: input.pullRequestUrl,
      draftSubmittedAt: submittedAt,
    },
    updatedAt: submittedAt,
    revision: bounty.revision + 1,
  };
}

export function expireRescueBountyOffer(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'offer expiry time');
  if (bounty.state !== 'open' || bounty.activeClaim) {
    throw new DomainRuleError('OFFER_NOT_EXPIRABLE', 'Bounty does not have an open offer');
  }
  if (timestampMs(command.at) < timestampMs(bounty.offerExpiresAt)) {
    throw new DomainRuleError('OFFER_STILL_ACTIVE', 'Bounty offer has not expired');
  }
  return {
    ...bounty,
    state: 'offer_refund_pending',
    updatedAt: new Date(timestampMs(command.at)).toISOString(),
    revision: bounty.revision + 1,
  };
}

export function finalizeRescueBountyOfferRefund(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'offer refund completion time');
  if (bounty.state !== 'offer_refund_pending' || bounty.activeClaim) {
    throw new DomainRuleError(
      'OFFER_REFUND_NOT_PENDING',
      'Bounty does not have an unclaimed offer awaiting refund',
    );
  }
  return {
    ...bounty,
    state: 'expired',
    updatedAt: new Date(timestampMs(command.at)).toISOString(),
    revision: bounty.revision + 1,
  };
}

export function expireRescueBountyClaim(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'claim expiry time');
  if (!['claimed', 'pr_submitted', 'validating'].includes(bounty.state)) {
    throw new DomainRuleError('CLAIM_NOT_EXPIRABLE', 'Bounty does not have an expirable claim');
  }
  const claim = requireActiveClaim(bounty);
  if (timestampMs(command.at) < timestampMs(claim.leaseExpiresAt)) {
    throw new DomainRuleError('CLAIM_STILL_ACTIVE', 'Claim lease has not expired');
  }

  const expiredAt = new Date(timestampMs(command.at)).toISOString();
  const expiredClaim: BountyClaim = {
    ...claim,
    state: 'expired',
    closedAt: expiredAt,
  };
  return {
    ...bounty,
    state: 'claim_refund_pending',
    activeClaim: expiredClaim,
    updatedAt: expiredAt,
    revision: bounty.revision + 1,
  };
}

export function finalizeRescueBountyClaimRefund(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'claim refund completion time');
  if (bounty.state !== 'claim_refund_pending' || bounty.activeClaim?.state !== 'expired') {
    throw new DomainRuleError(
      'CLAIM_REFUND_NOT_PENDING',
      'Bounty does not have an expired claim awaiting refund',
    );
  }
  const updatedAt = new Date(timestampMs(command.at)).toISOString();
  const expiredClaim = bounty.activeClaim;
  return {
    ...bounty,
    state: 'refunded',
    activeClaim: undefined,
    claimHistory: [...bounty.claimHistory, expiredClaim],
    updatedAt,
    revision: bounty.revision + 1,
  };
}

export function expireAcceptedRescueBountyRelease(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'release expiry time');
  if (bounty.state !== 'accepted') {
    throw new DomainRuleError('RELEASE_NOT_EXPIRABLE', 'Bounty release is not awaiting settlement');
  }
  const claim = requireActiveClaim(bounty);
  if (timestampMs(command.at) < timestampMs(claim.leaseExpiresAt)) {
    throw new DomainRuleError('CLAIM_STILL_ACTIVE', 'Claim lease has not expired');
  }
  return {
    ...bounty,
    state: 'release_refund_pending',
    activeClaim: {
      ...claim,
      state: 'expired',
      closedAt: new Date(timestampMs(command.at)).toISOString(),
    },
    updatedAt: new Date(timestampMs(command.at)).toISOString(),
    revision: bounty.revision + 1,
  };
}

export function finalizeExpiredReleaseRefund(
  bounty: RescueBounty,
  command: BountyCommand,
): RescueBounty {
  assertExpectedRevision(bounty.revision, command.expectedRevision);
  assertNotBefore(command.at, bounty.updatedAt, 'release refund completion time');
  if (bounty.state !== 'release_refund_pending' || bounty.activeClaim?.state !== 'expired') {
    throw new DomainRuleError(
      'RELEASE_REFUND_NOT_PENDING',
      'Bounty does not have an expired release awaiting refund',
    );
  }
  const updatedAt = new Date(timestampMs(command.at)).toISOString();
  return {
    ...bounty,
    state: 'refunded',
    activeClaim: undefined,
    claimHistory: [
      ...bounty.claimHistory,
      { ...bounty.activeClaim, state: 'refunded', closedAt: updatedAt },
    ],
    updatedAt,
    revision: bounty.revision + 1,
  };
}

function requireActiveClaim(bounty: RescueBounty): BountyClaim {
  if (!bounty.activeClaim) {
    throw new DomainRuleError('MISSING_ACTIVE_CLAIM', 'Bounty has no active claim');
  }
  return bounty.activeClaim;
}

export function normalizeBountyDisputeEvidence(
  evidence: BountyDisputeEvidence,
): BountyDisputeEvidence {
  const summary = evidence.summary.trim();
  if (summary.length < 20 || summary.length > 4_000) {
    throw new DomainRuleError(
      'INVALID_DISPUTE_EVIDENCE',
      'Dispute evidence summary must contain 20-4000 characters',
    );
  }
  if (evidence.references.length < 1 || evidence.references.length > 10) {
    throw new DomainRuleError(
      'INVALID_DISPUTE_EVIDENCE',
      'Dispute evidence must contain 1-10 references',
    );
  }
  const references = evidence.references.map((value) => {
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      throw new DomainRuleError('INVALID_DISPUTE_EVIDENCE', 'Evidence reference must be a URL');
    }
    if (url.protocol !== 'https:' || url.username || url.password || url.href.length > 2_000) {
      throw new DomainRuleError(
        'INVALID_DISPUTE_EVIDENCE',
        'Evidence references must be credential-free HTTPS URLs',
      );
    }
    return url.href;
  });
  if (new Set(references).size !== references.length) {
    throw new DomainRuleError('INVALID_DISPUTE_EVIDENCE', 'Evidence references must be unique');
  }
  return { summary, references };
}

export function fingerprintBountyDisputeEvidence(evidence: BountyDisputeEvidence): string {
  const normalized = normalizeBountyDisputeEvidence(evidence);
  return fingerprintPayload({
    summary: normalized.summary,
    references: normalized.references,
  });
}

function normalizeRepository(value: string): string {
  const repository = assertNonEmpty(value, 'repository').toLowerCase();
  if (!/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/.test(repository)) {
    throw new DomainRuleError('INVALID_REPOSITORY', 'Repository must use the owner/name format');
  }
  return repository;
}

function validateIssueUrl(value: string, repository: string, issueNumber: number): void {
  const normalizedRepository = normalizeRepository(repository);
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new DomainRuleError('INVALID_ISSUE_URL', 'Issue URL must be a valid URL');
  }
  if (
    url.protocol !== 'https:' ||
    url.hostname !== 'github.com' ||
    url.pathname.toLowerCase() !== `/${normalizedRepository}/issues/${issueNumber}` ||
    url.search ||
    url.hash
  ) {
    throw new DomainRuleError('INVALID_ISSUE_URL', 'Issue URL does not match the bounty issue');
  }
}

function validatePullRequestUrl(value: string, repository: string): void {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new DomainRuleError('INVALID_PULL_REQUEST_URL', 'Pull request URL must be valid');
  }
  if (
    url.protocol !== 'https:' ||
    url.hostname !== 'github.com' ||
    !new RegExp(`^/${escapeRegExp(repository)}/pull/[1-9][0-9]*$`, 'i').test(url.pathname) ||
    url.search ||
    url.hash
  ) {
    throw new DomainRuleError(
      'INVALID_PULL_REQUEST_URL',
      'Pull request must belong to the bounty repository',
    );
  }
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}
