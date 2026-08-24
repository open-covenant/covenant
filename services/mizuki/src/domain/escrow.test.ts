import { describe, expect, it } from 'vitest';
import {
  finalizeContributorEscrowBinding,
  prepareContributorEscrowBinding,
  createContributorEscrow,
  createEscrowOperation,
  transitionContributorEscrow,
  transitionEscrowOperation,
  type ContributorEscrow,
} from './escrow.js';
import { DomainRuleError } from './state-machine.js';

const T0 = '2026-08-22T10:00:00.000Z';

function requested(): ContributorEscrow {
  return createContributorEscrow({
    id: 'escrow-1',
    bountyId: 'bounty-1',
    repository: 'example/project',
    issueNumber: 1,
    issueTitle: 'Handle empty input',
    issueBody: 'The parser should accept empty input.',
    baseRef: 'main',
    baseSha: 'd'.repeat(40),
    reviewPolicy: { version: 1, model: 'independent-reviewer', maxFiles: 3 },
    amountCents: 1_000,
    acceptanceHash: 'a'.repeat(64),
    expiresAt: '2026-08-24T11:00:00.000Z',
    at: T0,
  });
}

function funded(): ContributorEscrow {
  const funding = transitionContributorEscrow(requested(), 'funding', {
    at: '2026-08-22T10:01:00.000Z',
    expectedRevision: 0,
  });
  return transitionContributorEscrow(funding, 'funded', {
    at: '2026-08-22T10:02:00.000Z',
    expectedRevision: 1,
    transactionSignature: 'funding-tx',
    reservationId: 'reservation-1',
    amountAtomic: '50000000',
  });
}

function bound(): ContributorEscrow {
  const pending = prepareContributorEscrowBinding(funded(), {
    at: '2026-08-22T10:03:00.000Z',
    expectedRevision: 2,
    claimId: 'claim-1',
    claimantId: 'github:42',
    claimantGithubLogin: 'maintainer',
    recipientWallet: 'wallet-1',
    claimExpiresAt: '2026-08-24T10:03:00.000Z',
    signature: 'wallet-proof',
  });
  return finalizeContributorEscrowBinding(pending, {
    at: '2026-08-22T10:04:00.000Z',
    expectedRevision: 3,
    bindOperationId: 'bind-1',
    transactionSignature: 'bind-tx',
  });
}

describe('contributor escrow', () => {
  it('records immutable offer terms before a claimant is known', () => {
    expect(requested()).toMatchObject({
      state: 'requested',
      bountyId: 'bounty-1',
      amountCents: 1_000,
      revision: 0,
    });
    expect(() =>
      createContributorEscrow({
        id: 'escrow-1',
        bountyId: 'bounty-1',
        repository: 'example/project',
        issueNumber: 1,
        issueTitle: 'Handle empty input',
        issueBody: 'The parser should accept empty input.',
        baseRef: 'main',
        baseSha: 'd'.repeat(40),
        reviewPolicy: { version: 1, model: 'independent-reviewer', maxFiles: 3 },
        acceptanceHash: 'a'.repeat(64),
        expiresAt: '2026-08-24T11:00:00.000Z',
        amountCents: 0,
        at: T0,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_ESCROW_AMOUNT',
      }),
    );
  });

  it('requires finalized transaction evidence for funding and release', () => {
    const funding = transitionContributorEscrow(requested(), 'funding', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    expect(() =>
      transitionContributorEscrow(funding, 'funded', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'MISSING_TRANSACTION',
      }),
    );
    expect(() =>
      transitionContributorEscrow(funding, 'funded', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
        transactionSignature: 'funding-tx',
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'MISSING_ESCROW_PRINCIPAL',
      }),
    );

    const releasePending = transitionContributorEscrow(bound(), 'release_pending', {
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 4,
    });
    const released = transitionContributorEscrow(releasePending, 'released', {
      at: '2026-08-22T10:06:00.000Z',
      expectedRevision: 5,
      transactionSignature: 'release-tx',
    });
    expect(released).toMatchObject({
      state: 'released',
      fundingSignature: 'funding-tx',
      amountAtomic: '50000000',
      bindSignature: 'bind-tx',
      releaseSignature: 'release-tx',
      revision: 6,
    });
  });

  it('supports a disputed refund path with explicit evidence', () => {
    let escrow = transitionContributorEscrow(bound(), 'disputed', {
      at: '2026-08-22T10:05:00.000Z',
      expectedRevision: 4,
      disputeId: 'dispute-1',
    });
    escrow = transitionContributorEscrow(escrow, 'refund_pending', {
      at: '2026-08-22T10:06:00.000Z',
      expectedRevision: 5,
    });
    escrow = transitionContributorEscrow(escrow, 'refunded', {
      at: '2026-08-22T10:07:00.000Z',
      expectedRevision: 6,
      transactionSignature: 'refund-tx',
    });
    expect(escrow).toMatchObject({
      state: 'refunded',
      disputeId: 'dispute-1',
      refundSignature: 'refund-tx',
      revision: 7,
    });
  });

  it('rejects missing dispute ids, terminal transitions, and stale commands', () => {
    expect(() =>
      transitionContributorEscrow(bound(), 'disputed', {
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 4,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'MISSING_DISPUTE' }));
    expect(() =>
      transitionContributorEscrow(bound(), 'release_pending', {
        at: '2026-08-22T10:05:00.000Z',
        expectedRevision: 3,
      }),
    ).toThrowError(expect.objectContaining<Partial<DomainRuleError>>({ code: 'STALE_REVISION' }));

    const failed = transitionContributorEscrow(requested(), 'failed', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    expect(() =>
      transitionContributorEscrow(failed, 'release_pending', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'INVALID_TRANSITION',
      }),
    );
  });
});

describe('escrow operations', () => {
  const fingerprint = 'a'.repeat(64);

  it('moves a typed operation through authorization, broadcast, and finality', () => {
    let operation = createEscrowOperation({
      id: 'operation-1',
      escrowId: 'escrow-1',
      kind: 'release',
      idempotencyKey: 'escrow:1:release',
      payloadFingerprint: fingerprint,
      at: T0,
    });
    operation = transitionEscrowOperation(operation, 'authorized', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    operation = transitionEscrowOperation(operation, 'broadcast', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: 1,
      transactionSignature: 'release-tx',
    });
    operation = transitionEscrowOperation(operation, 'finalized', {
      at: '2026-08-22T10:03:00.000Z',
      expectedRevision: 2,
    });
    expect(operation).toMatchObject({
      state: 'finalized',
      transactionSignature: 'release-tx',
      revision: 3,
    });
  });

  it('requires transaction and failure evidence at the relevant transitions', () => {
    const operation = transitionEscrowOperation(
      createEscrowOperation({
        id: 'operation-1',
        escrowId: 'escrow-1',
        kind: 'create',
        idempotencyKey: 'escrow:1:create',
        payloadFingerprint: fingerprint,
        at: T0,
      }),
      'authorized',
      {
        at: '2026-08-22T10:01:00.000Z',
        expectedRevision: 0,
      },
    );
    expect(() =>
      transitionEscrowOperation(operation, 'broadcast', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'MISSING_TRANSACTION' }),
    );
    expect(() =>
      transitionEscrowOperation(operation, 'failed', {
        at: '2026-08-22T10:02:00.000Z',
        expectedRevision: 1,
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({ code: 'MISSING_FAILURE_CODE' }),
    );
  });

  it('does not finalize a different transaction than the one broadcast', () => {
    let operation = createEscrowOperation({
      id: 'operation-1',
      escrowId: 'escrow-1',
      kind: 'release',
      idempotencyKey: 'escrow:1:release',
      payloadFingerprint: fingerprint,
      at: T0,
    });
    operation = transitionEscrowOperation(operation, 'authorized', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    operation = transitionEscrowOperation(operation, 'broadcast', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: 1,
      transactionSignature: 'broadcast-tx',
    });
    expect(() =>
      transitionEscrowOperation(operation, 'finalized', {
        at: '2026-08-22T10:03:00.000Z',
        expectedRevision: 2,
        transactionSignature: 'other-tx',
      }),
    ).toThrowError(
      expect.objectContaining<Partial<DomainRuleError>>({
        code: 'TRANSACTION_MISMATCH',
      }),
    );
  });

  it('allows an intentional retry from failed while preserving the operation identity', () => {
    let operation = createEscrowOperation({
      id: 'operation-1',
      escrowId: 'escrow-1',
      kind: 'refund',
      idempotencyKey: 'escrow:1:refund',
      payloadFingerprint: fingerprint,
      at: T0,
    });
    operation = transitionEscrowOperation(operation, 'authorized', {
      at: '2026-08-22T10:01:00.000Z',
      expectedRevision: 0,
    });
    operation = transitionEscrowOperation(operation, 'failed', {
      at: '2026-08-22T10:02:00.000Z',
      expectedRevision: 1,
      failureCode: 'RPC_TIMEOUT',
    });
    operation = transitionEscrowOperation(operation, 'authorized', {
      at: '2026-08-22T10:03:00.000Z',
      expectedRevision: 2,
    });
    expect(operation).toMatchObject({
      id: 'operation-1',
      idempotencyKey: 'escrow:1:refund',
      state: 'authorized',
      revision: 3,
    });
  });
});
