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

export const registerRefundLiabilityRequestSchema = refundRequestSchema;

export const dischargeRefundLiabilityRequestSchema = z
  .object({
    jobId: externalIdSchema,
    settlementSignature: signatureSchema,
    repository: repositorySchema,
    pullRequestNumber: z.number().int().positive().max(2_147_483_647),
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
    pullRequestNumber: z.number().int().positive().max(2_147_483_647),
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
export type CreateEscrowRequest = z.infer<typeof createEscrowRequestSchema>;
export type BindChallengeRequest = z.infer<typeof bindChallengeRequestSchema>;
export type GitHubIdentityGrantRequest = z.infer<typeof githubIdentityGrantRequestSchema>;
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

export interface RefundLiability {
  id: string;
  idempotencyKey: string;
  requestHash: string;
  jobId: string;
  settlementSignature: string;
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
  settlementSignature: string;
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
  escrowAuthority: string;
  finalizedEscrowBalanceLamports: string | null;
  availableEscrowReserveLamports: string | null;
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
    settlementSignature: liability.settlementSignature,
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

export function refundAuthorizationMessage(
  action: 'register' | 'execute',
  request: Pick<RefundRequest, 'jobId' | 'settlementSignature' | 'authorizationExpiresAt'>,
): string {
  const title =
    action === 'register'
      ? 'Mizuki refund liability registration'
      : 'Mizuki refund execution authorization';
  return [
    title,
    'Version: 1',
    `Job: ${request.jobId}`,
    `Settlement: ${request.settlementSignature}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

export function refundDischargeAuthorizationMessage(
  request: Pick<
    DischargeRefundLiabilityRequest,
    'jobId' | 'settlementSignature' | 'repository' | 'pullRequestNumber' | 'authorizationExpiresAt'
  >,
): string {
  return [
    'Mizuki refund liability discharge authorization',
    'Version: 1',
    `Job: ${request.jobId}`,
    `Settlement: ${request.settlementSignature}`,
    `Repository: ${request.repository.toLowerCase()}`,
    `Pull Request: ${request.pullRequestNumber}`,
    `Expires At: ${new Date(request.authorizationExpiresAt).toISOString()}`,
  ].join('\n');
}

function stringDetail(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

export function requestHash(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
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
