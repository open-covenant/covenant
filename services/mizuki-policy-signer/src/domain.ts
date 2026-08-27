import { createHash } from 'node:crypto';
import { z } from 'zod';

export const base58Schema = z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/);
export const signatureSchema = z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{64,88}$/);
export const externalIdSchema = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
export const hashSchema = z.string().regex(/^[a-f0-9]{64}$/);
export const gitCommitShaSchema = z.string().regex(/^[a-f0-9]{40,64}$/);
export const gitRefSchema = z
  .string()
  .min(1)
  .max(255)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._/-]*$/);
export const repositorySchema = z
  .string()
  .min(3)
  .max(201)
  .regex(/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/)
  .transform((value) => value.toLowerCase());
export const githubLoginSchema = z
  .string()
  .min(1)
  .max(39)
  .regex(/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/)
  .transform((value) => value.toLowerCase());
export const PAYMENT_AUTHORIZATION_MAX_BYTES = 12_000;

export const refundRequestSchema = z
  .object({
    jobId: externalIdSchema,
    settlementSignature: signatureSchema,
    authorizationExpiresAt: z.string().datetime({ offset: true }),
    authorizationSignature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const registerRefundLiabilityRequestSchema = refundRequestSchema
  .extend({
    repositoryAdmissionId: z.string().uuid(),
    repositoryAdmissionEvidenceHash: hashSchema,
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    baseRef: gitRefSchema,
    baseSha: gitCommitShaSchema,
    repositoryAuthorizedAt: z.string().datetime({ offset: true }),
    authorizationEvidenceHash: hashSchema,
  })
  .strict();

export const dischargeRefundLiabilityRequestSchema = z
  .object({
    jobId: externalIdSchema,
    settlementSignature: signatureSchema,
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    pullRequestNumber: z.number().int().positive().max(2_147_483_647),
    deliveredCommitSha: gitCommitShaSchema,
    reviewedHeadSha: gitCommitShaSchema,
    reviewedBaseSha: gitCommitShaSchema,
    reviewedBaseRef: gitRefSchema,
    reviewedDiffHash: hashSchema,
    authorizationExpiresAt: z.string().datetime({ offset: true }),
    authorizationSignature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const bindRefundLiabilityDeliveryRequestSchema = z
  .object({
    jobId: externalIdSchema,
    settlementSignature: signatureSchema,
    reviewedHeadSha: gitCommitShaSchema,
    reviewedBaseSha: gitCommitShaSchema,
    reviewedBaseRef: gitRefSchema,
    reviewedDiffHash: hashSchema,
    authorizationExpiresAt: z.string().datetime({ offset: true }),
    authorizationSignature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const createEscrowRequestSchema = z
  .object({
    bountyId: externalIdSchema,
    amountUsdCents: z.number().int().positive(),
    acceptanceHash: hashSchema,
    expiresAt: z.string().datetime({ offset: true }),
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    issueTitle: z.string().min(1).max(512),
    issueBody: z.string().max(100_000),
    baseRef: gitRefSchema,
    baseSha: gitCommitShaSchema,
    reviewPolicy: z
      .object({
        version: z.literal(1),
        model: z
          .string()
          .min(1)
          .max(256)
          .regex(/^\S(?:.*\S)?$/),
        maxFiles: z.number().int().positive().max(20),
      })
      .strict(),
  })
  .strict();

export const bindChallengeRequestSchema = z
  .object({
    claimantWallet: base58Schema,
    githubGrantId: z.string().uuid(),
  })
  .strict();

export const githubIdentityGrantRequestSchema = z
  .object({
    accessToken: z.string().min(20).max(255).regex(/^\S+$/),
  })
  .strict();

export const repositoryReadinessRequestSchema = z
  .object({
    repository: repositorySchema,
  })
  .strict();

const repositoryAdmissionBaseSchema = z
  .object({
    quoteId: z.string().uuid(),
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    baseRef: gitRefSchema,
    baseSha: gitCommitShaSchema,
    reservationKeyHash: hashSchema,
  })
  .strict();

export const repositoryAdmissionRequestSchema = repositoryAdmissionBaseSchema
  .extend({
    paymentAuthorization: z
      .string()
      .min(1)
      .max(PAYMENT_AUTHORIZATION_MAX_BYTES)
      .regex(/^[A-Za-z0-9+/]*={0,2}$/),
  })
  .strict();

export const validateRepositoryAdmissionRequestSchema = repositoryAdmissionBaseSchema
  .extend({ paymentAuthorizationHash: hashSchema, evidenceHash: hashSchema })
  .strict();

export const reconcileRepositorySettlementRequestSchema = z
  .object({
    evidenceHash: hashSchema,
  })
  .strict();

export const createPaymentIntentRequestSchema = z
  .object({
    jobId: externalIdSchema,
    repositoryAdmissionId: z.string().uuid(),
    repositoryAdmissionEvidenceHash: hashSchema,
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    baseRef: gitRefSchema,
    baseSha: gitCommitShaSchema,
    repositoryAuthorizedAt: z.string().datetime({ offset: true }),
    authorizationEvidenceHash: hashSchema,
    bountyAmountUsdCents: z.number().int().positive(),
    authorizationExpiresAt: z.string().datetime({ offset: true }),
    authorizationSignature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const activatePaymentIntentRequestSchema = z
  .object({ settlementSignature: signatureSchema })
  .strict();

export const reconcilePaymentIntentRequestSchema = z.object({}).strict();

export const x402PaymentAuthorizationSchema = z
  .object({
    x402Version: z.literal(2),
    resource: z
      .object({
        url: z.string().url().max(2_048),
      })
      .passthrough(),
    accepted: z
      .object({
        scheme: z.literal('exact'),
        network: z.literal('solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp'),
        asset: base58Schema,
        amount: z.string().regex(/^[1-9]\d*$/),
        payTo: base58Schema,
        maxTimeoutSeconds: z.literal(300),
        extra: z
          .object({
            feePayer: base58Schema,
          })
          .passthrough(),
      })
      .passthrough(),
    payload: z
      .object({
        transaction: z
          .string()
          .min(1)
          .max(4_096)
          .regex(/^[A-Za-z0-9+/]*={0,2}$/),
      })
      .strict(),
  })
  .passthrough();

export const bindEscrowRequestSchema = z
  .object({
    challengeId: z.string().uuid(),
    signature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const releaseEscrowRequestSchema = z
  .object({
    repository: repositorySchema,
    issueNumber: z.number().int().positive().max(2_147_483_647),
    pullRequestNumber: z.number().int().positive().max(2_147_483_647),
    mergeCommitSha: gitCommitShaSchema,
    reviewedHeadSha: gitCommitShaSchema,
    reviewedBaseSha: gitCommitShaSchema,
    reviewedBaseRef: gitRefSchema,
    reviewedDiffHash: hashSchema,
    reviewReceiptId: z.string().uuid(),
    reviewReceiptHash: hashSchema,
    reviewModel: z
      .string()
      .min(1)
      .max(256)
      .regex(/^\S(?:.*\S)?$/),
    reviewRoute: z.literal('marketplace'),
    reviewedAt: z.string().datetime({ offset: true }),
    authorizationExpiresAt: z.string().datetime({ offset: true }),
    authorizationSignature: z
      .string()
      .regex(/^[A-Za-z0-9+/]{86}==$/)
      .refine((value) => Buffer.from(value, 'base64').length === 64),
  })
  .strict();

export const refundEscrowRequestSchema = z
  .object({
    reasonCode: z.enum(['expired', 'rejected', 'dispute_resolved']),
  })
  .strict();

export type RefundRequest = z.infer<typeof refundRequestSchema>;
export type RegisterRefundLiabilityRequest = z.infer<typeof registerRefundLiabilityRequestSchema>;
export type DischargeRefundLiabilityRequest = z.infer<typeof dischargeRefundLiabilityRequestSchema>;
export type BindRefundLiabilityDeliveryRequest = z.infer<
  typeof bindRefundLiabilityDeliveryRequestSchema
>;
export type CreateEscrowRequest = z.infer<typeof createEscrowRequestSchema>;
export type BindChallengeRequest = z.infer<typeof bindChallengeRequestSchema>;
export type GitHubIdentityGrantRequest = z.infer<typeof githubIdentityGrantRequestSchema>;
export type RepositoryAdmissionRequest = z.infer<typeof repositoryAdmissionRequestSchema>;
export type ValidateRepositoryAdmissionRequest = z.infer<
  typeof validateRepositoryAdmissionRequestSchema
>;
export type ReconcileRepositorySettlementRequest = z.infer<
  typeof reconcileRepositorySettlementRequestSchema
>;
export type CreatePaymentIntentRequest = z.infer<typeof createPaymentIntentRequestSchema>;
export type ActivatePaymentIntentRequest = z.infer<typeof activatePaymentIntentRequestSchema>;
export type X402PaymentAuthorization = z.infer<typeof x402PaymentAuthorizationSchema>;
export type BindEscrowRequest = z.infer<typeof bindEscrowRequestSchema>;
export type ReleaseEscrowRequest = z.infer<typeof releaseEscrowRequestSchema>;
export type RefundEscrowRequest = z.infer<typeof refundEscrowRequestSchema>;

export type OperationKind =
  | 'refund'
  | 'escrow_reserve'
  | 'escrow_bind'
  | 'escrow_release'
  | 'escrow_refund';

export type SpendBucket = 'refund' | 'escrow' | 'none';

export type OperationStatus =
  | 'reserved'
  | 'prepared'
  | 'broadcasting'
  | 'submitted'
  | 'reconciling'
  | 'finalized'
  | 'rejected';

export interface PreparedTransaction {
  signature: string;
  wireTransaction: string;
  lastValidBlockHeight: number;
  derived: Record<string, string>;
}

export interface OperationRecord {
  id: string;
  idempotencyKey: string;
  resourceKey: string;
  requestHash: string;
  kind: OperationKind;
  status: OperationStatus;
  amountUsdCents: number;
  spendBucket: SpendBucket;
  asset: string;
  recipient: string;
  details: Record<string, unknown>;
  prepared: PreparedTransaction | null;
  transactionSignature: string | null;
  errorCode: string | null;
  errorMessage: string | null;
  leaseOwner: string | null;
  leaseExpiresAt: Date | null;
  createdAt: Date;
  updatedAt: Date;
  version: number;
}

export interface ReserveOperation {
  id: string;
  idempotencyKey: string;
  resourceKey: string;
  requestHash: string;
  kind: OperationKind;
  amountUsdCents: number;
  spendBucket: SpendBucket;
  asset: string;
  recipient: string;
  details: Record<string, unknown>;
}

export interface SettlementFacts {
  signature: string;
  payer: string;
  recipient: string;
  mint: string;
  rawAmount: string;
  decimals: number;
  finalized: boolean;
  succeeded: boolean;
  slot: number;
  blockTimeUnixSeconds: number;
}

export interface SettlementAuthorization {
  wireTransaction: string;
  feePayer: string;
  rawAmount: string;
  notBeforeUnixSeconds: number;
}

export interface SettlementAuthorizationBinding {
  messageHash: string;
  clientSignature: string;
  feePayer: string;
  rawAmount: string;
  notBeforeUnixSeconds: number;
  notAfterUnixSeconds: number;
}

export interface RepositoryAdmission {
  id: string;
  idempotencyKey: string;
  requestHash: string;
  quoteId: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  reservationKeyHash: string;
  paymentAuthorizationHash: string;
  settlementMessageHash: string;
  settlementClientSignature: string;
  settlementFeePayer: string;
  settlementPayer: string | null;
  settlementMemo: string | null;
  settlementRawAmount: string;
  paymentWindowStartUnixSeconds: number;
  paymentWindowEndUnixSeconds: number;
  verifierAppId: string;
  installationId: number;
  repositorySelection: 'selected';
  permissions: {
    checks: 'read';
    contents: 'read';
    issues: 'read';
    metadata: 'read';
    pull_requests: 'read';
    statuses: 'read';
  };
  tokenRepositories: 1;
  tokenExpiresAt: Date;
  admittedAt: Date;
  evidenceHash: string;
}

export type PaymentIntentStatus = 'reserved' | 'activated' | 'expired_unpaid';

export interface PaymentIntent {
  id: string;
  idempotencyKey: string;
  requestHash: string;
  jobId: string;
  quoteId: string;
  repositoryAdmissionId: string;
  repositoryAdmissionEvidenceHash: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  repositoryAuthorizedAt: Date;
  authorizationEvidenceHash: string;
  payer: string;
  payee: string;
  mint: string;
  rawAmount: string;
  amountUsdCents: number;
  bountyAmountUsdCents: number;
  bountyReserveLamports: string;
  memo: string;
  signedMessageHash: string;
  payerSignature: string;
  paymentWindowStartUnixSeconds: number;
  paymentWindowEndUnixSeconds: number;
  status: PaymentIntentStatus;
  settlementSignature: string | null;
  liabilityId: string | null;
  activationIdempotencyKey: string | null;
  createdAt: Date;
  activatedAt: Date | null;
  expiredAt: Date | null;
}

export interface PaymentIntentView {
  id: string;
  jobId: string;
  quoteId: string;
  repositoryAdmissionId: string;
  status: PaymentIntentStatus;
  payer: string;
  payee: string;
  mint: string;
  rawAmount: string;
  amountUsdCents: number;
  bountyAmountUsdCents: number;
  bountyReserveLamports: string;
  memo: string;
  paymentWindowStartUnixSeconds: number;
  paymentWindowEndUnixSeconds: number;
  settlementSignature: string | null;
  liabilityId: string | null;
  createdAt: string;
  activatedAt: string | null;
  expiredAt: string | null;
}

export interface PaymentIntentActivationView {
  paymentIntent: PaymentIntentView;
  refundLiability: RefundLiabilityView;
}

export type RefundCommandStatus = 'pending' | 'submitted' | 'finalized' | 'indeterminate';

export interface RefundCommand {
  id: string;
  idempotencyKey: string;
  requestHash: string;
  liabilityId: string;
  jobId: string;
  status: RefundCommandStatus;
  currentOperationId: string | null;
  attemptCount: number;
  createdAt: Date;
  updatedAt: Date;
}

export function refundCommandView(command: RefundCommand) {
  return {
    id: command.id,
    jobId: command.jobId,
    liabilityId: command.liabilityId,
    status: command.status,
    currentOperationId: command.currentOperationId,
    attemptCount: command.attemptCount,
    createdAt: command.createdAt.toISOString(),
    updatedAt: command.updatedAt.toISOString(),
  };
}

export interface RepositoryAdmissionView {
  id: string;
  quoteId: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  reservationKeyHash: string;
  paymentAuthorizationHash: string;
  verifierAppId: string;
  installationId: number;
  repositorySelection: 'selected';
  permissions: RepositoryAdmission['permissions'];
  tokenRepositories: 1;
  tokenExpiresAt: string;
  admittedAt: string;
  evidenceHash: string;
}

export interface RefundLiability {
  id: string;
  idempotencyKey: string;
  requestHash: string;
  jobId: string;
  repositoryAdmissionId: string;
  settlementSignature: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  repositoryAuthorizedAt: Date;
  authorizationEvidenceHash: string;
  reviewedHeadSha: string | null;
  reviewedBaseSha: string | null;
  reviewedBaseRef: string | null;
  reviewedDiffHash: string | null;
  deliveryBoundAt: Date | null;
  deliveryBindingIdempotencyKey: string | null;
  deliveryBindingRequestHash: string | null;
  deliveryBindingHash: string | null;
  payer: string;
  treasury: string;
  mint: string;
  rawAmount: string;
  decimals: number;
  amountUsdCents: number;
  settlementSlot: number;
  settlementBlockTimeUnixSeconds: number;
  createdAt: Date;
  dischargedAt: Date | null;
  dischargeEvidenceHash: string | null;
  dischargeEvidence: Record<string, unknown> | null;
  dischargeIdempotencyKey: string | null;
  dischargeRequestHash: string | null;
}

export interface RefundLiabilityView {
  id: string;
  jobId: string;
  repositoryAdmissionId: string;
  settlementSignature: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  repositoryAuthorizedAt: string;
  authorizationEvidenceHash: string;
  reviewedHeadSha: string | null;
  reviewedBaseSha: string | null;
  reviewedBaseRef: string | null;
  reviewedDiffHash: string | null;
  deliveryBoundAt: string | null;
  deliveryBindingHash: string | null;
  payer: string;
  mint: string;
  rawAmount: string;
  decimals: number;
  amountUsdCents: number;
  settlementSlot: number;
  settlementBlockTimeUnixSeconds: number;
  createdAt: string;
  dischargedAt: string | null;
  dischargeEvidenceHash: string | null;
}

export type ChainOperation =
  | {
      kind: 'refund';
      intentId: string;
      payer: string;
      mint: string;
      rawAmount: string;
      decimals: number;
    }
  | {
      kind: 'escrow_reserve';
      intentId: string;
      bountyDigest: string;
      amountLamports: string;
      expiresAtUnixSeconds: string;
      acceptanceHash: string;
    }
  | {
      kind: 'escrow_bind';
      intentId: string;
      bountyDigest: string;
      claimantWallet: string;
      claimExpiresAtUnixSeconds: string;
      bindingEvidence: string;
    }
  | {
      kind: 'escrow_release' | 'escrow_refund';
      intentId: string;
      bountyDigest: string;
      claimantWallet?: string;
      resolutionEvidence: string;
    };

export type TransactionState = 'missing' | 'submitted' | 'finalized' | 'failed';

export interface OperationView {
  id: string;
  kind: OperationKind;
  status: OperationStatus;
  amountUsdCents: number;
  amountAtomic: string | null;
  asset: string;
  recipient: string;
  reservationId: string | null;
  bountyDigest: string | null;
  escrowAddress: string | null;
  vaultAddress: string | null;
  guardAddress: string | null;
  transactionSignature: string | null;
  error: { code: string; message: string } | null;
  createdAt: string;
  updatedAt: string;
}

export interface BindChallenge {
  id: string;
  escrowOperationId: string;
  bindingHash: string;
  claimantWallet: string;
  claimantGitHubId: string;
  claimantGitHubLogin: string;
  message: string;
  claimExpiresAt: Date;
  issuedAt: Date;
  expiresAt: Date;
  consumedAt: Date | null;
  bindOperationId: string | null;
}

export interface BindChallengeView {
  id: string;
  message: string;
  expiresAt: string;
  claimExpiresAt: string;
}

export interface GitHubIdentityGrant {
  id: string;
  githubId: string;
  login: string;
  issuedAt: Date;
  expiresAt: Date;
  consumedAt: Date | null;
  challengeId: string | null;
}

export interface GitHubIdentityGrantView {
  id: string;
  githubId: string;
  login: string;
  expiresAt: string;
}

export interface RefundReadinessView {
  healthy: boolean;
  refundTreasury: string;
  refundMint: string;
  refundDecimals: number;
  finalizedBalanceRaw: string | null;
  pendingRefundRaw: string | null;
  treasuryAvailableRefundRaw: string | null;
  remainingRefundLimitUsdCents: number | null;
  availableRefundRaw: string | null;
  refundSignerLamports: string | null;
  refundFeeReserveLamports: string;
  refundAtaRentLamports: string | null;
  pendingRefundCount: number | null;
  availableRefundTransactions: number | null;
  escrowRollingLimitUsdCents: number;
  rollingEscrowSpendUsdCents: number | null;
  remainingEscrowLimitUsdCents: number | null;
  escrowAuthority: string;
  finalizedEscrowBalanceLamports: string | null;
  availableEscrowReserveLamports: string | null;
}

export interface SignerReadinessEvidence {
  healthy: boolean;
  observedAt: string;
  checks: {
    database: boolean;
    rpcConsensus: boolean;
    priceConsensus: boolean;
    githubCredential: boolean;
    independentReviewer: boolean;
    escrowProgram: boolean;
    refundCustody: boolean;
    bountyCustody: boolean;
  };
  chain: {
    rpcProviders: 2;
    escrowProgramId: string;
    escrowProgramDataSha256: string;
    escrowProgramImmutable: true;
    refundTreasury: string;
    refundMint: string;
    refundDecimals: number;
    refundRawAmount: string;
    refundSignerLamports: string;
    refundAtaRentLamports: string;
    escrowAuthority: string;
    escrowLamports: string;
    availableEscrowReserveLamports: string;
  } | null;
  prices: {
    feedCount: 2;
    priceUsdMicros: number;
    observedAt: string;
    observations: Array<{
      feed: 'primary' | 'secondary';
      priceUsdMicros: number;
      observedAt: string;
    }>;
  } | null;
}

export function paymentIntentView(intent: PaymentIntent): PaymentIntentView {
  return {
    id: intent.id,
    jobId: intent.jobId,
    quoteId: intent.quoteId,
    repositoryAdmissionId: intent.repositoryAdmissionId,
    status: intent.status,
    payer: intent.payer,
    payee: intent.payee,
    mint: intent.mint,
    rawAmount: intent.rawAmount,
    amountUsdCents: intent.amountUsdCents,
    bountyAmountUsdCents: intent.bountyAmountUsdCents,
    bountyReserveLamports: intent.bountyReserveLamports,
    memo: intent.memo,
    paymentWindowStartUnixSeconds: intent.paymentWindowStartUnixSeconds,
    paymentWindowEndUnixSeconds: intent.paymentWindowEndUnixSeconds,
    settlementSignature: intent.settlementSignature,
    liabilityId: intent.liabilityId,
    createdAt: intent.createdAt.toISOString(),
    activatedAt: intent.activatedAt?.toISOString() ?? null,
    expiredAt: intent.expiredAt?.toISOString() ?? null,
  };
}

export function paymentIntentAuthorizationMessage(
  request: Omit<CreatePaymentIntentRequest, 'authorizationSignature'>,
): string {
  return [
    'Mizuki payment intent authorization',
    'Version: 1',
    `Job: ${request.jobId}`,
    `Repository Admission: ${request.repositoryAdmissionId}`,
    `Repository Admission Evidence: ${request.repositoryAdmissionEvidenceHash}`,
    `Repository: ${request.repository.toLowerCase()}`,
    `Issue: ${request.issueNumber}`,
    `Base Ref: ${request.baseRef}`,
    `Base SHA: ${request.baseSha}`,
    `Repository Authorized At: ${new Date(request.repositoryAuthorizedAt).toISOString()}`,
    `Authorization Evidence: ${request.authorizationEvidenceHash}`,
    `Bounty Amount USD Cents: ${request.bountyAmountUsdCents}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

export function operationView(record: OperationRecord): OperationView {
  return {
    id: record.id,
    kind: record.kind,
    status: record.status,
    amountUsdCents: record.amountUsdCents,
    amountAtomic:
      record.kind === 'escrow_reserve' ? stringDetail(record.details.amountLamports) : null,
    asset: record.asset,
    recipient: record.recipient,
    reservationId:
      record.kind === 'escrow_reserve'
        ? record.id
        : typeof record.details.escrowOperationId === 'string'
          ? record.details.escrowOperationId
          : null,
    bountyDigest: stringDetail(record.details.bountyDigest),
    escrowAddress: stringDetail(record.details.escrowAddress),
    vaultAddress: stringDetail(record.details.vaultAddress),
    guardAddress: stringDetail(record.details.guardAddress),
    transactionSignature: record.transactionSignature,
    error:
      record.errorCode && record.errorMessage
        ? { code: record.errorCode, message: record.errorMessage }
        : null,
    createdAt: record.createdAt.toISOString(),
    updatedAt: record.updatedAt.toISOString(),
  };
}

export function refundLiabilityView(liability: RefundLiability): RefundLiabilityView {
  return {
    id: liability.id,
    jobId: liability.jobId,
    repositoryAdmissionId: liability.repositoryAdmissionId,
    settlementSignature: liability.settlementSignature,
    repository: liability.repository,
    issueNumber: liability.issueNumber,
    baseRef: liability.baseRef,
    baseSha: liability.baseSha,
    repositoryAuthorizedAt: liability.repositoryAuthorizedAt.toISOString(),
    authorizationEvidenceHash: liability.authorizationEvidenceHash,
    reviewedHeadSha: liability.reviewedHeadSha,
    reviewedBaseSha: liability.reviewedBaseSha,
    reviewedBaseRef: liability.reviewedBaseRef,
    reviewedDiffHash: liability.reviewedDiffHash,
    deliveryBoundAt: liability.deliveryBoundAt?.toISOString() ?? null,
    deliveryBindingHash: liability.deliveryBindingHash,
    payer: liability.payer,
    mint: liability.mint,
    rawAmount: liability.rawAmount,
    decimals: liability.decimals,
    amountUsdCents: liability.amountUsdCents,
    settlementSlot: liability.settlementSlot,
    settlementBlockTimeUnixSeconds: liability.settlementBlockTimeUnixSeconds,
    createdAt: liability.createdAt.toISOString(),
    dischargedAt: liability.dischargedAt?.toISOString() ?? null,
    dischargeEvidenceHash: liability.dischargeEvidenceHash,
  };
}

export function repositoryAdmissionView(admission: RepositoryAdmission): RepositoryAdmissionView {
  return {
    id: admission.id,
    quoteId: admission.quoteId,
    repository: admission.repository,
    issueNumber: admission.issueNumber,
    baseRef: admission.baseRef,
    baseSha: admission.baseSha,
    reservationKeyHash: admission.reservationKeyHash,
    paymentAuthorizationHash: admission.paymentAuthorizationHash,
    verifierAppId: admission.verifierAppId,
    installationId: admission.installationId,
    repositorySelection: admission.repositorySelection,
    permissions: { ...admission.permissions },
    tokenRepositories: admission.tokenRepositories,
    tokenExpiresAt: admission.tokenExpiresAt.toISOString(),
    admittedAt: admission.admittedAt.toISOString(),
    evidenceHash: admission.evidenceHash,
  };
}

export function refundAuthorizationMessage(
  action: 'register' | 'execute',
  request:
    | Pick<RefundRequest, 'jobId' | 'settlementSignature' | 'authorizationExpiresAt'>
    | RegisterRefundLiabilityRequest,
): string {
  const title =
    action === 'register'
      ? 'Mizuki refund liability registration'
      : 'Mizuki refund execution authorization';
  const fields = [
    title,
    `Version: ${action === 'register' ? 3 : 1}`,
    `Job: ${request.jobId}`,
    `Settlement: ${request.settlementSignature}`,
  ];
  if (action === 'register') {
    const registration = request as RegisterRefundLiabilityRequest;
    fields.push(
      `Repository Admission: ${registration.repositoryAdmissionId}`,
      `Repository Admission Evidence: ${registration.repositoryAdmissionEvidenceHash}`,
      `Repository: ${registration.repository.toLowerCase()}`,
      `Issue: ${registration.issueNumber}`,
      `Base Ref: ${registration.baseRef}`,
      `Base SHA: ${registration.baseSha}`,
      `Repository Authorized At: ${new Date(registration.repositoryAuthorizedAt).toISOString()}`,
      `Authorization Evidence: ${registration.authorizationEvidenceHash}`,
    );
  }
  fields.push(`Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`);
  return fields.join('\n');
}

export function refundDischargeAuthorizationMessage(
  request: Pick<
    DischargeRefundLiabilityRequest,
    | 'jobId'
    | 'settlementSignature'
    | 'repository'
    | 'issueNumber'
    | 'pullRequestNumber'
    | 'deliveredCommitSha'
    | 'reviewedHeadSha'
    | 'reviewedBaseSha'
    | 'reviewedBaseRef'
    | 'reviewedDiffHash'
    | 'authorizationExpiresAt'
  >,
): string {
  return [
    'Mizuki refund liability discharge authorization',
    'Version: 2',
    `Job: ${request.jobId}`,
    `Settlement: ${request.settlementSignature}`,
    `Repository: ${request.repository.toLowerCase()}`,
    `Issue: ${request.issueNumber}`,
    `Pull Request: ${request.pullRequestNumber}`,
    `Delivered Commit: ${request.deliveredCommitSha}`,
    `Reviewed Head: ${request.reviewedHeadSha}`,
    `Reviewed Base SHA: ${request.reviewedBaseSha}`,
    `Reviewed Base Ref: ${request.reviewedBaseRef}`,
    `Reviewed Diff: ${request.reviewedDiffHash}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

export function refundDeliveryBindingAuthorizationMessage(
  request: Pick<
    BindRefundLiabilityDeliveryRequest,
    | 'jobId'
    | 'settlementSignature'
    | 'reviewedHeadSha'
    | 'reviewedBaseSha'
    | 'reviewedBaseRef'
    | 'reviewedDiffHash'
    | 'authorizationExpiresAt'
  >,
): string {
  return [
    'Mizuki refund liability delivery binding',
    'Version: 1',
    `Job: ${request.jobId}`,
    `Settlement: ${request.settlementSignature}`,
    `Reviewed Head: ${request.reviewedHeadSha}`,
    `Reviewed Base SHA: ${request.reviewedBaseSha}`,
    `Reviewed Base Ref: ${request.reviewedBaseRef}`,
    `Reviewed Diff: ${request.reviewedDiffHash}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

export function escrowReleaseAuthorizationMessage(
  escrowOperationId: string,
  request: Pick<
    ReleaseEscrowRequest,
    | 'repository'
    | 'issueNumber'
    | 'pullRequestNumber'
    | 'mergeCommitSha'
    | 'reviewedHeadSha'
    | 'reviewedBaseSha'
    | 'reviewedBaseRef'
    | 'reviewedDiffHash'
    | 'reviewReceiptId'
    | 'reviewReceiptHash'
    | 'reviewModel'
    | 'reviewRoute'
    | 'reviewedAt'
    | 'authorizationExpiresAt'
  >,
): string {
  return [
    'Mizuki escrow release authorization',
    'Version: 1',
    `Escrow: ${escrowOperationId}`,
    `Repository: ${request.repository.toLowerCase()}`,
    `Issue: ${request.issueNumber}`,
    `Pull Request: ${request.pullRequestNumber}`,
    `Merge Commit: ${request.mergeCommitSha}`,
    `Reviewed Head: ${request.reviewedHeadSha}`,
    `Reviewed Base SHA: ${request.reviewedBaseSha}`,
    `Reviewed Base Ref: ${request.reviewedBaseRef}`,
    `Reviewed Diff: ${request.reviewedDiffHash}`,
    `Review Receipt: ${request.reviewReceiptId}`,
    `Review Receipt Hash: ${request.reviewReceiptHash}`,
    `Review Model: ${request.reviewModel}`,
    `Review Route: ${request.reviewRoute}`,
    `Reviewed At: ${new Date(request.reviewedAt).toISOString()}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

function stringDetail(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

export function requestHash(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

export function escrowAcceptanceHash(request: Omit<CreateEscrowRequest, 'acceptanceHash'>): string {
  return requestHash({ kind: 'mizuki_contributor_escrow_acceptance', ...request });
}

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;

  const entries = Object.entries(value as Record<string, unknown>)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`);
  return `{${entries.join(',')}}`;
}

export class PolicyError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly statusCode: number,
    readonly retryable = false,
  ) {
    super(message);
    this.name = 'PolicyError';
  }
}
