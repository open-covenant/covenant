import {
  DomainRuleError,
  assertExpectedRevision,
  assertNonEmpty,
  assertNotBefore,
  assertTransition,
  assertUsdCents,
  timestampMs,
  type TransitionTable,
} from './state-machine.js';
import { normalizeIdempotencyKey } from './idempotency.js';

export type ContributorEscrowState =
  | 'requested'
  | 'funding'
  | 'funded'
  | 'bind_pending'
  | 'bound'
  | 'release_pending'
  | 'released'
  | 'refund_pending'
  | 'refunded'
  | 'disputed'
  | 'failed';

export type EscrowOperationKind =
  | 'create'
  | 'release'
  | 'refund'
  | 'open_dispute'
  | 'resolve_dispute';

export type EscrowOperationState =
  | 'prepared'
  | 'authorized'
  | 'broadcast'
  | 'finalized'
  | 'rejected'
  | 'failed';

export type ContributorEscrow = {
  id: string;
  bountyId: string;
  repository: string;
  issueNumber: number;
  amountCents: number;
  acceptanceHash: string;
  expiresAt: string;
  state: ContributorEscrowState;
  reservationId?: string;
  amountAtomic?: string;
  fundingSignature?: string;
  claimId?: string;
  claimantId?: string;
  claimantGithubLogin?: string;
  recipientWallet?: string;
  claimExpiresAt?: string;
  claimSignature?: string;
  bindOperationId?: string;
  bindSignature?: string;
  releaseSignature?: string;
  refundSignature?: string;
  refundReasonCode?: 'expired' | 'rejected' | 'dispute_resolved';
  disputeId?: string;
  createdAt: string;
  updatedAt: string;
  revision: number;
};

export type EscrowOperation = {
  id: string;
  escrowId: string;
  kind: EscrowOperationKind;
  state: EscrowOperationState;
  idempotencyKey: string;
  payloadFingerprint: string;
  transactionSignature?: string;
  failureCode?: string;
  createdAt: string;
  updatedAt: string;
  revision: number;
};

const escrowTransitions: TransitionTable<ContributorEscrowState> = {
  requested: ['funding', 'failed'],
  funding: ['funded', 'failed'],
  funded: ['bind_pending', 'refund_pending', 'failed'],
  bind_pending: ['bound', 'failed'],
  bound: ['release_pending', 'refund_pending', 'disputed'],
  release_pending: ['released', 'refund_pending', 'failed', 'disputed'],
  released: [],
  refund_pending: ['released', 'refunded', 'failed', 'disputed'],
  refunded: [],
  disputed: ['release_pending', 'refund_pending', 'released', 'refunded', 'failed'],
  failed: [],
};

const operationTransitions: TransitionTable<EscrowOperationState> = {
  prepared: ['authorized', 'rejected'],
  authorized: ['broadcast', 'failed', 'rejected'],
  broadcast: ['finalized', 'failed'],
  finalized: [],
  rejected: [],
  failed: ['authorized', 'broadcast', 'rejected'],
};

export function createContributorEscrow(input: {
  id: string;
  bountyId: string;
  repository: string;
  issueNumber: number;
  amountCents: number;
  acceptanceHash: string;
  expiresAt: string;
  at: string;
}): ContributorEscrow {
  const amountCents = assertUsdCents(input.amountCents, 'escrow amount');
  if (amountCents === 0) {
    throw new DomainRuleError('INVALID_ESCROW_AMOUNT', 'Escrow amount must be greater than zero');
  }
  const createdAt = new Date(timestampMs(input.at, 'created at')).toISOString();
  const expiresAt = new Date(timestampMs(input.expiresAt, 'escrow expiry')).toISOString();
  if (expiresAt <= createdAt) {
    throw new DomainRuleError('INVALID_ESCROW_EXPIRY', 'Escrow expiry must follow creation');
  }
  if (!/^[a-f0-9]{64}$/i.test(input.acceptanceHash)) {
    throw new DomainRuleError('INVALID_ACCEPTANCE_HASH', 'Acceptance hash must be SHA-256');
  }
  if (!/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/i.test(input.repository)) {
    throw new DomainRuleError('INVALID_REPOSITORY', 'Repository must use the owner/name format');
  }
  if (!Number.isSafeInteger(input.issueNumber) || input.issueNumber <= 0) {
    throw new DomainRuleError('INVALID_ISSUE', 'Issue number must be a positive integer');
  }
  return {
    id: assertNonEmpty(input.id, 'escrow id'),
    bountyId: assertNonEmpty(input.bountyId, 'bounty id'),
    repository: input.repository.toLowerCase(),
    issueNumber: input.issueNumber,
    amountCents,
    acceptanceHash: input.acceptanceHash.toLowerCase(),
    expiresAt,
    state: 'requested',
    createdAt,
    updatedAt: createdAt,
    revision: 0,
  };
}

export function transitionContributorEscrow(
  escrow: ContributorEscrow,
  to: ContributorEscrowState,
  input: {
    at: string;
    expectedRevision: number;
    transactionSignature?: string;
    policyOperationId?: string;
    reservationId?: string;
    amountAtomic?: string;
    disputeId?: string;
    refundReasonCode?: 'expired' | 'rejected' | 'dispute_resolved';
  },
): ContributorEscrow {
  assertExpectedRevision(escrow.revision, input.expectedRevision);
  assertNotBefore(input.at, escrow.updatedAt, 'escrow transition time');
  assertTransition(escrowTransitions, escrow.state, to, 'Contributor escrow');
  if (to === 'bound') {
    throw new DomainRuleError(
      'ESCROW_BIND_COMMAND_REQUIRED',
      'Escrow binding requires immutable claimant evidence',
    );
  }
  if (
    escrow.state === 'release_pending' &&
    to === 'refund_pending' &&
    (!escrow.claimExpiresAt || timestampMs(input.at) < timestampMs(escrow.claimExpiresAt))
  ) {
    throw new DomainRuleError(
      'RELEASE_STILL_ACTIVE',
      'A pending release cannot become refundable before the immutable claim expiry',
    );
  }

  const signatureField = signatureFieldForState(to);
  if (signatureField && !input.transactionSignature) {
    throw new DomainRuleError('MISSING_TRANSACTION', `${to} requires a transaction signature`);
  }
  if (to === 'disputed' && !input.disputeId) {
    throw new DomainRuleError('MISSING_DISPUTE', 'Disputed escrow requires a dispute id');
  }
  if (to === 'funded' && !/^[1-9][0-9]*$/.test(input.amountAtomic ?? '')) {
    throw new DomainRuleError(
      'MISSING_ESCROW_PRINCIPAL',
      'Funded escrow requires an exact positive atomic principal',
    );
  }

  const updatedAt = new Date(timestampMs(input.at)).toISOString();
  return {
    ...escrow,
    state: to,
    ...(signatureField && input.transactionSignature
      ? { [signatureField]: assertNonEmpty(input.transactionSignature, 'transaction signature') }
      : {}),
    ...(input.disputeId ? { disputeId: assertNonEmpty(input.disputeId, 'dispute id') } : {}),
    ...(to === 'funded' && input.amountAtomic ? { amountAtomic: input.amountAtomic } : {}),
    ...((input.reservationId ?? input.policyOperationId)
      ? {
          reservationId: assertNonEmpty(
            input.reservationId ?? input.policyOperationId!,
            'reservation id',
          ),
        }
      : {}),
    ...(input.refundReasonCode ? { refundReasonCode: input.refundReasonCode } : {}),
    updatedAt,
    revision: escrow.revision + 1,
  };
}

export function prepareContributorEscrowBinding(
  escrow: ContributorEscrow,
  input: {
    at: string;
    expectedRevision: number;
    claimId: string;
    claimantId: string;
    claimantGithubLogin: string;
    recipientWallet: string;
    claimExpiresAt: string;
    signature: string;
  },
): ContributorEscrow {
  assertExpectedRevision(escrow.revision, input.expectedRevision);
  assertNotBefore(input.at, escrow.updatedAt, 'escrow binding preparation time');
  assertTransition(escrowTransitions, escrow.state, 'bind_pending', 'Contributor escrow');
  if (!escrow.reservationId || !escrow.fundingSignature) {
    throw new DomainRuleError('ESCROW_NOT_RESERVED', 'Escrow must be funded before binding');
  }
  const updatedAt = new Date(timestampMs(input.at)).toISOString();
  const claimExpiresAt = new Date(timestampMs(input.claimExpiresAt, 'claim expiry')).toISOString();
  if (timestampMs(claimExpiresAt) <= timestampMs(updatedAt)) {
    throw new DomainRuleError('INVALID_CLAIM_EXPIRY', 'Claim expiry must follow binding');
  }
  return {
    ...escrow,
    state: 'bind_pending',
    claimId: assertNonEmpty(input.claimId, 'claim id'),
    claimantId: assertNonEmpty(input.claimantId, 'claimant id'),
    claimantGithubLogin: assertNonEmpty(
      input.claimantGithubLogin,
      'claimant GitHub login',
    ).toLowerCase(),
    recipientWallet: assertNonEmpty(input.recipientWallet, 'recipient wallet'),
    claimExpiresAt,
    claimSignature: assertNonEmpty(input.signature, 'claim signature'),
    updatedAt,
    revision: escrow.revision + 1,
  };
}

export function finalizeContributorEscrowBinding(
  escrow: ContributorEscrow,
  input: {
    at: string;
    expectedRevision: number;
    bindOperationId: string;
    transactionSignature: string;
  },
): ContributorEscrow {
  assertExpectedRevision(escrow.revision, input.expectedRevision);
  assertNotBefore(input.at, escrow.updatedAt, 'escrow binding completion time');
  assertTransition(escrowTransitions, escrow.state, 'bound', 'Contributor escrow');
  if (
    !escrow.claimId ||
    !escrow.claimantId ||
    !escrow.claimantGithubLogin ||
    !escrow.recipientWallet ||
    !escrow.claimExpiresAt ||
    !escrow.claimSignature
  ) {
    throw new DomainRuleError('MISSING_BINDING_EVIDENCE', 'Pending bind evidence is incomplete');
  }
  return {
    ...escrow,
    state: 'bound',
    bindOperationId: assertNonEmpty(input.bindOperationId, 'bind operation id'),
    bindSignature: assertNonEmpty(input.transactionSignature, 'bind transaction signature'),
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: escrow.revision + 1,
  };
}

export function createEscrowOperation(input: {
  id: string;
  escrowId: string;
  kind: EscrowOperationKind;
  idempotencyKey: string;
  payloadFingerprint: string;
  at: string;
}): EscrowOperation {
  const createdAt = new Date(timestampMs(input.at, 'created at')).toISOString();
  if (!/^[a-f0-9]{64}$/i.test(input.payloadFingerprint)) {
    throw new DomainRuleError('INVALID_FINGERPRINT', 'Payload fingerprint must be SHA-256');
  }
  return {
    id: assertNonEmpty(input.id, 'operation id'),
    escrowId: assertNonEmpty(input.escrowId, 'escrow id'),
    kind: input.kind,
    state: 'prepared',
    idempotencyKey: normalizeIdempotencyKey(input.idempotencyKey),
    payloadFingerprint: input.payloadFingerprint.toLowerCase(),
    createdAt,
    updatedAt: createdAt,
    revision: 0,
  };
}

export function transitionEscrowOperation(
  operation: EscrowOperation,
  to: EscrowOperationState,
  input: {
    at: string;
    expectedRevision: number;
    transactionSignature?: string;
    failureCode?: string;
  },
): EscrowOperation {
  assertExpectedRevision(operation.revision, input.expectedRevision);
  assertNotBefore(input.at, operation.updatedAt, 'operation transition time');
  assertTransition(operationTransitions, operation.state, to, 'Escrow operation');
  if (
    operation.state === 'broadcast' &&
    input.transactionSignature &&
    operation.transactionSignature !== input.transactionSignature
  ) {
    throw new DomainRuleError(
      'TRANSACTION_MISMATCH',
      'Finality evidence does not match the broadcast transaction',
    );
  }
  if (
    (to === 'broadcast' || to === 'finalized') &&
    !(input.transactionSignature || operation.transactionSignature)
  ) {
    throw new DomainRuleError('MISSING_TRANSACTION', `${to} requires a transaction signature`);
  }
  if ((to === 'failed' || to === 'rejected') && !input.failureCode) {
    throw new DomainRuleError('MISSING_FAILURE_CODE', `${to} requires a failure code`);
  }

  return {
    ...operation,
    state: to,
    ...(input.transactionSignature
      ? {
          transactionSignature: assertNonEmpty(input.transactionSignature, 'transaction signature'),
        }
      : {}),
    ...(input.failureCode
      ? { failureCode: assertNonEmpty(input.failureCode, 'failure code') }
      : {}),
    updatedAt: new Date(timestampMs(input.at)).toISOString(),
    revision: operation.revision + 1,
  };
}

function signatureFieldForState(
  state: ContributorEscrowState,
): 'fundingSignature' | 'releaseSignature' | 'refundSignature' | undefined {
  if (state === 'funded') return 'fundingSignature';
  if (state === 'released') return 'releaseSignature';
  if (state === 'refunded') return 'refundSignature';
  return undefined;
}
