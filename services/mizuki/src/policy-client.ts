import { createHash, createPrivateKey, sign, type KeyObject } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { z } from 'zod';
import type { Config } from './config.js';
import type { Quote, RepositoryAdmissionReceipt } from './types.js';

const operationSchema = z.object({
  id: z.string().uuid(),
  kind: z.enum(['refund', 'escrow_reserve', 'escrow_bind', 'escrow_release', 'escrow_refund']),
  status: z.enum([
    'reserved',
    'prepared',
    'broadcasting',
    'submitted',
    'reconciling',
    'finalized',
    'rejected',
  ]),
  amountUsdCents: z.number().int().nonnegative(),
  amountAtomic: z
    .string()
    .regex(/^[0-9]+$/)
    .nullable(),
  asset: z.string(),
  recipient: z.string(),
  transactionSignature: z.string().nullable(),
  error: z.object({ code: z.string(), message: z.string() }).nullable(),
  createdAt: z.string(),
  updatedAt: z.string(),
});

export type PolicyOperation = z.infer<typeof operationSchema>;

const bindChallengeSchema = z.object({
  id: z.string().uuid(),
  message: z.string().min(1),
  expiresAt: z.string().datetime({ offset: true }),
  claimExpiresAt: z.string().datetime({ offset: true }),
});

export type BindChallenge = z.infer<typeof bindChallengeSchema>;

const githubIdentityGrantSchema = z.object({
  id: z.string().uuid(),
  githubId: z.string().min(1),
  login: z.string().min(1),
  expiresAt: z.string().datetime({ offset: true }),
});

export type GithubIdentityGrant = z.infer<typeof githubIdentityGrantSchema>;

const readinessSchema = z
  .object({
    healthy: z.boolean(),
    refundTreasury: z.string().min(1),
    refundMint: z.string().min(1),
    refundDecimals: z.number().int().min(0).max(18),
    finalizedBalanceRaw: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
    pendingRefundRaw: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
    treasuryAvailableRefundRaw: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
    remainingRefundLimitUsdCents: z.number().int().nonnegative().nullable(),
    availableRefundRaw: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
    escrowAuthority: z.string().min(1),
    finalizedEscrowBalanceLamports: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
    availableEscrowReserveLamports: z
      .string()
      .regex(/^[0-9]+$/)
      .nullable(),
  })
  .strict();

export type PolicyReadiness = z.infer<typeof readinessSchema>;

const repositoryIdentitySchema = z
  .string()
  .min(3)
  .max(201)
  .regex(/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/)
  .transform((value) => value.toLowerCase());
const githubAppIdSchema = z
  .string()
  .regex(/^[1-9]\d{0,15}$/)
  .refine((value) => Number.isSafeInteger(Number(value)));
const repositoryReadinessSchema = z
  .object({
    ready: z.literal(true),
    repository: repositoryIdentitySchema,
    verifierAppId: githubAppIdSchema,
    installationId: z.number().int().positive().safe(),
    repositorySelection: z.literal('selected'),
    permissions: z
      .object({
        checks: z.literal('read'),
        contents: z.literal('read'),
        issues: z.literal('read'),
        metadata: z.literal('read'),
        pull_requests: z.literal('read'),
        statuses: z.literal('read'),
      })
      .strict(),
    tokenRepositories: z.literal(1),
    tokenExpiresAt: z.string().datetime({ offset: true }),
  })
  .strict();

export type RepositoryReadiness = z.infer<typeof repositoryReadinessSchema>;

const repositoryAdmissionSchema = z
  .object({
    id: z.string().uuid(),
    quoteId: z.string().uuid(),
    repository: repositoryIdentitySchema,
    issueNumber: z.number().int().positive(),
    baseRef: z.string().min(1).max(255),
    baseSha: z.string().regex(/^[a-f0-9]{40,64}$/),
    reservationKeyHash: z.string().regex(/^[a-f0-9]{64}$/),
    paymentAuthorizationHash: z.string().regex(/^[a-f0-9]{64}$/),
    verifierAppId: githubAppIdSchema,
    installationId: z.number().int().positive().safe(),
    repositorySelection: z.literal('selected'),
    permissions: z
      .object({
        checks: z.literal('read'),
        contents: z.literal('read'),
        issues: z.literal('read'),
        metadata: z.literal('read'),
        pull_requests: z.literal('read'),
        statuses: z.literal('read'),
      })
      .strict(),
    tokenRepositories: z.literal(1),
    tokenExpiresAt: z.string().datetime({ offset: true }),
    admittedAt: z.string().datetime({ offset: true }),
    evidenceHash: z.string().regex(/^[a-f0-9]{64}$/),
  })
  .strict();

const repositoryAdmissionBindingSchema = repositoryAdmissionSchema.pick({
  quoteId: true,
  repository: true,
  issueNumber: true,
  baseRef: true,
  baseSha: true,
  reservationKeyHash: true,
  paymentAuthorizationHash: true,
});

const settlementEvidenceSchema = z
  .object({
    signature: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{64,88}$/),
    payer: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    recipient: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    mint: z.string().regex(/^[1-9A-HJ-NP-Za-km-z]{32,44}$/),
    rawAmount: z.string().regex(/^[1-9]\d*$/),
    decimals: z.number().int().nonnegative().max(18),
    finalized: z.literal(true),
    succeeded: z.literal(true),
    slot: z.number().int().nonnegative().safe(),
    blockTimeUnixSeconds: z.number().int().safe(),
  })
  .strict();

export type SettlementEvidence = z.infer<typeof settlementEvidenceSchema>;

export interface RepositoryAdmissionBinding {
  quoteId: string;
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  reservationKeyHash: string;
  paymentAuthorizationHash: string;
}

export function repositoryAdmissionBinding(
  quote: Quote,
  reservationKey: string,
  paymentAuthorization: string,
): RepositoryAdmissionBinding {
  return {
    quoteId: quote.id,
    repository: `${quote.owner}/${quote.repo}`.toLowerCase(),
    issueNumber: quote.issueNumber,
    baseRef: quote.defaultBranch,
    baseSha: quote.baseSha,
    reservationKeyHash: sha256(reservationKey),
    paymentAuthorizationHash: sha256(paymentAuthorization),
  };
}

const refundLiabilitySchema = z.object({
  id: z.string().uuid(),
  jobId: z.string().min(1),
  repositoryAdmissionId: z.string().uuid(),
  settlementSignature: z.string().min(1),
  repository: z.string().min(3),
  issueNumber: z.number().int().positive(),
  baseRef: z.string().min(1),
  baseSha: z.string().regex(/^[a-f0-9]{40,64}$/),
  repositoryAuthorizedAt: z.string().datetime({ offset: true }),
  authorizationEvidenceHash: z.string().regex(/^[a-f0-9]{64}$/),
  reviewedHeadSha: z
    .string()
    .regex(/^[a-f0-9]{40,64}$/)
    .nullable(),
  reviewedBaseSha: z
    .string()
    .regex(/^[a-f0-9]{40,64}$/)
    .nullable(),
  reviewedBaseRef: z.string().min(1).nullable(),
  reviewedDiffHash: z
    .string()
    .regex(/^[a-f0-9]{64}$/)
    .nullable(),
  deliveryBoundAt: z.string().datetime({ offset: true }).nullable(),
  deliveryBindingHash: z
    .string()
    .regex(/^[a-f0-9]{64}$/)
    .nullable(),
  payer: z.string().min(1),
  mint: z.string().min(1),
  rawAmount: z.string().regex(/^[0-9]+$/),
  decimals: z.number().int().nonnegative(),
  amountUsdCents: z.number().int().positive(),
  settlementSlot: z.number().int().nonnegative(),
  settlementBlockTimeUnixSeconds: z.number().int(),
  createdAt: z.string().datetime({ offset: true }),
  dischargedAt: z.string().datetime({ offset: true }).nullable(),
  dischargeEvidenceHash: z
    .string()
    .regex(/^[a-f0-9]{64}$/)
    .nullable(),
});

export type RefundLiability = z.infer<typeof refundLiabilitySchema>;

export interface RefundLiabilityCommitment {
  repository: string;
  issueNumber: number;
  baseRef: string;
  baseSha: string;
  repositoryAuthorizedAt: string;
  authorizationEvidenceHash: string;
}

export interface RefundLiabilityDischarge {
  jobId: string;
  settlementSignature: string;
  repository: string;
  issueNumber: number;
  pullRequestNumber: number;
  deliveredCommitSha: string;
  reviewedHeadSha: string;
  reviewedBaseSha: string;
  reviewedBaseRef: string;
  reviewedDiffHash: string;
}

export interface RefundLiabilityDeliveryBinding {
  jobId: string;
  settlementSignature: string;
  reviewedHeadSha: string;
  reviewedBaseSha: string;
  reviewedBaseRef: string;
  reviewedDiffHash: string;
}

export function refundLiabilityCommitment(quote: Quote): RefundLiabilityCommitment {
  const authorization = quote.authorizationReceipt;
  if (!authorization) throw new Error('repository authorization evidence is required');
  if (!Number.isFinite(Date.parse(authorization.authorizedAt))) {
    throw new Error('repository authorization timestamp is invalid');
  }
  return {
    repository: `${quote.owner}/${quote.repo}`.toLowerCase(),
    issueNumber: quote.issueNumber,
    baseRef: quote.defaultBranch,
    baseSha: quote.baseSha,
    repositoryAuthorizedAt: new Date(authorization.authorizedAt).toISOString(),
    authorizationEvidenceHash: authorization.evidenceHash,
  };
}

export interface RefundCapacityPolicy {
  readiness(): Promise<PolicyReadiness>;
}

export interface PaymentPolicy extends RefundCapacityPolicy {
  assertRepositoryReady(repository: string): Promise<RepositoryReadiness>;
  createRepositoryAdmission(
    binding: RepositoryAdmissionBinding,
    paymentAuthorization: string,
  ): Promise<RepositoryAdmissionReceipt>;
  validateRepositoryAdmission(
    receipt: RepositoryAdmissionReceipt,
    binding: RepositoryAdmissionBinding,
  ): Promise<RepositoryAdmissionReceipt>;
  reconcileRepositorySettlement(receipt: RepositoryAdmissionReceipt): Promise<SettlementEvidence>;
  registerRefundLiability(
    jobId: string,
    settlementSignature: string,
    commitment: RefundLiabilityCommitment,
    admission: RepositoryAdmissionReceipt,
  ): Promise<RefundLiability>;
  bindRefundLiabilityDelivery(
    liabilityId: string,
    input: RefundLiabilityDeliveryBinding,
  ): Promise<RefundLiability>;
  dischargeRefundLiability(
    liabilityId: string,
    input: RefundLiabilityDischarge,
  ): Promise<RefundLiability>;
  refund(jobId: string, settlementSignature: string): Promise<PolicyOperation>;
}

export interface GithubIdentityRegistrar {
  registerGithubIdentity(accessToken: string): Promise<GithubIdentityGrant>;
}

export interface FinancialPolicy extends PaymentPolicy {
  reserveEscrow(input: {
    bountyId: string;
    repository: string;
    issueNumber: number;
    issueTitle: string;
    issueBody: string;
    baseRef: string;
    baseSha: string;
    reviewPolicy: { version: 1; model: string; maxFiles: number };
    amountUsdCents: number;
    acceptanceHash: string;
    expiresAt: string;
  }): Promise<PolicyOperation>;
  createBindChallenge(
    reservationId: string,
    input: { claimantWallet: string; githubGrantId: string },
  ): Promise<BindChallenge>;
  bindEscrow(
    reservationId: string,
    challengeId: string,
    signature: string,
  ): Promise<PolicyOperation>;
  releaseEscrow(
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
  ): Promise<PolicyOperation>;
  refundEscrow(
    operationId: string,
    reasonCode: 'expired' | 'rejected' | 'dispute_resolved',
  ): Promise<PolicyOperation>;
}

export class PolicySignerClient implements FinancialPolicy {
  private readonly jobAuthorityKey?: KeyObject;

  constructor(
    private readonly config: Pick<
      Config,
      'policySignerUrl' | 'policySignerToken' | 'jobAuthoritySeed' | 'githubAppId'
    >,
    private readonly request: typeof fetch = fetch,
    private readonly waitMs = 60_000,
    private readonly now: () => Date = () => new Date(),
  ) {
    if (config.jobAuthoritySeed) {
      const seed = Buffer.from(config.jobAuthoritySeed, 'base64');
      if (seed.length !== 32 || seed.toString('base64') !== config.jobAuthoritySeed) {
        throw new Error('job authority seed must be canonical base64 for 32 bytes');
      }
      const pkcs8 = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed]);
      this.jobAuthorityKey = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
    }
  }

  async registerRefundLiability(
    jobId: string,
    settlementSignature: string,
    commitment: RefundLiabilityCommitment,
    admission: RepositoryAdmissionReceipt,
  ): Promise<RefundLiability> {
    const parsedAdmission = repositoryAdmissionSchema.parse(admission);
    this.assertAdmissionCommitment(parsedAdmission, commitment);
    const body = this.refundAuthorization(
      'register',
      jobId,
      settlementSignature,
      commitment,
      parsedAdmission,
    );
    return refundLiabilitySchema.parse(
      await this.callJson('/v1/refund-liabilities', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': `mizuki-refund-liability-${jobId}`,
        },
        body: JSON.stringify(body),
      }),
    );
  }

  async refund(jobId: string, settlementSignature: string): Promise<PolicyOperation> {
    return this.mutate(
      '/v1/refunds',
      `mizuki-refund-${jobId}`,
      this.refundAuthorization('execute', jobId, settlementSignature),
    );
  }

  async bindRefundLiabilityDelivery(
    liabilityId: string,
    input: RefundLiabilityDeliveryBinding,
  ): Promise<RefundLiability> {
    const body = this.deliveryBindingAuthorization(input);
    return refundLiabilitySchema.parse(
      await this.callJson(`/v1/refund-liabilities/${liabilityId}/delivery-bindings`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': `mizuki-refund-liability-delivery-${input.jobId}`,
        },
        body: JSON.stringify(body),
      }),
    );
  }

  async dischargeRefundLiability(
    liabilityId: string,
    input: RefundLiabilityDischarge,
  ): Promise<RefundLiability> {
    const body = this.dischargeAuthorization(input);
    return refundLiabilitySchema.parse(
      await this.callJson(`/v1/refund-liabilities/${liabilityId}/discharge`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': `mizuki-refund-liability-discharge-${input.jobId}`,
        },
        body: JSON.stringify(body),
      }),
    );
  }

  async readiness(): Promise<PolicyReadiness> {
    return readinessSchema.parse(await this.callJson('/v1/readiness'));
  }

  async assertRepositoryReady(repository: string): Promise<RepositoryReadiness> {
    const normalized = repositoryIdentitySchema.parse(repository);
    if (!this.config.githubAppId) throw new Error('delivery GitHub App is not configured');
    const deliveryAppId = githubAppIdSchema.parse(this.config.githubAppId);
    const evidence = repositoryReadinessSchema.parse(
      await this.callJson('/v1/readiness/repository', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ repository: normalized }),
      }),
    );
    if (evidence.repository !== normalized) {
      throw new Error('policy verifier returned readiness for a different repository');
    }
    if (evidence.verifierAppId === deliveryAppId) {
      throw new Error('policy verifier App must be distinct from the delivery App');
    }
    if (Date.parse(evidence.tokenExpiresAt) <= this.now().getTime()) {
      throw new Error('policy verifier returned expired repository readiness evidence');
    }
    return evidence;
  }

  async createRepositoryAdmission(
    binding: RepositoryAdmissionBinding,
    paymentAuthorization: string,
  ): Promise<RepositoryAdmissionReceipt> {
    const normalized = repositoryAdmissionBindingSchema.parse(binding);
    if (sha256(paymentAuthorization) !== normalized.paymentAuthorizationHash) {
      throw new Error('payment authorization does not match the repository admission binding');
    }
    const { paymentAuthorizationHash: _, ...requestBinding } = normalized;
    const receipt = repositoryAdmissionSchema.parse(
      await this.callJson('/v1/repository-admissions', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': `mizuki-repository-admission-${normalized.quoteId}`,
        },
        body: JSON.stringify({ ...requestBinding, paymentAuthorization }),
      }),
    );
    this.assertAdmission(receipt, normalized, true);
    return receipt;
  }

  async validateRepositoryAdmission(
    receipt: RepositoryAdmissionReceipt,
    binding: RepositoryAdmissionBinding,
  ): Promise<RepositoryAdmissionReceipt> {
    const parsedReceipt = repositoryAdmissionSchema.parse(receipt);
    const normalized = repositoryAdmissionBindingSchema.parse(binding);
    this.assertAdmission(parsedReceipt, normalized, false);
    const stored = repositoryAdmissionSchema.parse(
      await this.callJson(`/v1/repository-admissions/${parsedReceipt.id}/validate`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ ...normalized, evidenceHash: parsedReceipt.evidenceHash }),
      }),
    );
    if (!isDeepStrictEqual(stored, parsedReceipt)) {
      throw new Error('policy verifier returned a different repository admission');
    }
    this.assertAdmission(stored, normalized, false);
    return stored;
  }

  async reconcileRepositorySettlement(
    receipt: RepositoryAdmissionReceipt,
  ): Promise<SettlementEvidence> {
    const parsedReceipt = repositoryAdmissionSchema.parse(receipt);
    return settlementEvidenceSchema.parse(
      await this.callJson(`/v1/repository-admissions/${parsedReceipt.id}/settlements/reconcile`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ evidenceHash: parsedReceipt.evidenceHash }),
      }),
    );
  }

  async registerGithubIdentity(accessToken: string): Promise<GithubIdentityGrant> {
    const body = await this.callJson('/v1/github/identity-grants', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ accessToken }),
    });
    return githubIdentityGrantSchema.parse(body);
  }

  async reserveEscrow(input: {
    bountyId: string;
    repository: string;
    issueNumber: number;
    issueTitle: string;
    issueBody: string;
    baseRef: string;
    baseSha: string;
    reviewPolicy: { version: 1; model: string; maxFiles: number };
    amountUsdCents: number;
    acceptanceHash: string;
    expiresAt: string;
  }): Promise<PolicyOperation> {
    return this.mutate('/v1/escrows', `mizuki-escrow-${input.bountyId}`, input);
  }

  async createBindChallenge(
    reservationId: string,
    input: { claimantWallet: string; githubGrantId: string },
  ): Promise<BindChallenge> {
    const body = await this.callJson(`/v1/escrows/${reservationId}/bind-challenges`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
    });
    return bindChallengeSchema.parse(body);
  }

  async bindEscrow(
    reservationId: string,
    challengeId: string,
    signature: string,
  ): Promise<PolicyOperation> {
    return this.mutate(
      `/v1/escrows/${reservationId}/bind`,
      `mizuki-escrow-bind-${reservationId}-${challengeId}`,
      { challengeId, signature },
    );
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
    return this.mutate(
      `/v1/escrows/${operationId}/release`,
      `mizuki-escrow-release-${operationId}`,
      this.escrowReleaseAuthorization(operationId, input),
    );
  }

  async refundEscrow(
    operationId: string,
    reasonCode: 'expired' | 'rejected' | 'dispute_resolved',
  ): Promise<PolicyOperation> {
    return this.mutate(
      `/v1/escrows/${operationId}/refund`,
      `mizuki-escrow-refund-${operationId}-${reasonCode}`,
      { reasonCode },
    );
  }

  private async mutate(
    path: string,
    idempotencyKey: string,
    body: Record<string, unknown>,
  ): Promise<PolicyOperation> {
    const operation = await this.call(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': idempotencyKey },
      body: JSON.stringify(body),
    });
    return operation.status === 'finalized' ? operation : this.wait(operation.id);
  }

  private assertAdmission(
    receipt: RepositoryAdmissionReceipt,
    binding: RepositoryAdmissionBinding,
    requireFresh: boolean,
  ): void {
    if (!isDeepStrictEqual(admissionBindingFromReceipt(receipt), binding)) {
      throw new Error('repository admission does not match the settlement reservation');
    }
    if (!this.config.githubAppId) throw new Error('delivery GitHub App is not configured');
    if (receipt.verifierAppId === githubAppIdSchema.parse(this.config.githubAppId)) {
      throw new Error('policy verifier App must be distinct from the delivery App');
    }
    if (requireFresh && Date.parse(receipt.tokenExpiresAt) <= this.now().getTime()) {
      throw new Error('policy verifier returned expired repository admission');
    }
  }

  private refundAuthorization(
    action: 'register' | 'execute',
    jobId: string,
    settlementSignature: string,
    commitment?: RefundLiabilityCommitment,
    admission?: RepositoryAdmissionReceipt,
  ): Record<string, string | number> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    if (action === 'register' && !commitment) {
      throw new Error('refund liability commitment is required');
    }
    if (action === 'register' && !admission) {
      throw new Error('repository admission is required');
    }
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const title =
      action === 'register'
        ? 'Mizuki refund liability registration'
        : 'Mizuki refund execution authorization';
    const message = [
      title,
      `Version: ${action === 'register' ? 3 : 1}`,
      `Job: ${jobId}`,
      `Settlement: ${settlementSignature}`,
    ];
    if (action === 'register') {
      message.push(
        `Repository Admission: ${admission!.id}`,
        `Repository Admission Evidence: ${admission!.evidenceHash}`,
        `Repository: ${commitment!.repository.toLowerCase()}`,
        `Issue: ${commitment!.issueNumber}`,
        `Base Ref: ${commitment!.baseRef}`,
        `Base SHA: ${commitment!.baseSha}`,
        `Repository Authorized At: ${new Date(commitment!.repositoryAuthorizedAt).toISOString()}`,
        `Authorization Evidence: ${commitment!.authorizationEvidenceHash}`,
      );
    }
    message.push(`Expires At: ${authorizationExpiresAt}`);
    const normalizedCommitment = commitment
      ? { ...commitment, repository: commitment.repository.toLowerCase() }
      : {};
    return {
      jobId,
      settlementSignature,
      ...(admission
        ? {
            repositoryAdmissionId: admission.id,
            repositoryAdmissionEvidenceHash: admission.evidenceHash,
          }
        : {}),
      ...normalizedCommitment,
      authorizationExpiresAt,
      authorizationSignature: sign(
        null,
        Buffer.from(message.join('\n'), 'utf8'),
        this.jobAuthorityKey,
      ).toString('base64'),
    };
  }

  private assertAdmissionCommitment(
    admission: RepositoryAdmissionReceipt,
    commitment: RefundLiabilityCommitment,
  ): void {
    if (
      admission.repository !== commitment.repository.toLowerCase() ||
      admission.issueNumber !== commitment.issueNumber ||
      admission.baseRef !== commitment.baseRef ||
      admission.baseSha !== commitment.baseSha
    ) {
      throw new Error('repository admission does not match the refund liability commitment');
    }
  }

  private deliveryBindingAuthorization(
    input: RefundLiabilityDeliveryBinding,
  ): Record<string, string> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const message = [
      'Mizuki refund liability delivery binding',
      'Version: 1',
      `Job: ${input.jobId}`,
      `Settlement: ${input.settlementSignature}`,
      `Reviewed Head: ${input.reviewedHeadSha}`,
      `Reviewed Base SHA: ${input.reviewedBaseSha}`,
      `Reviewed Base Ref: ${input.reviewedBaseRef}`,
      `Reviewed Diff: ${input.reviewedDiffHash}`,
      `Expires At: ${authorizationExpiresAt}`,
    ].join('\n');
    return {
      ...input,
      authorizationExpiresAt,
      authorizationSignature: sign(
        null,
        Buffer.from(message, 'utf8'),
        this.jobAuthorityKey,
      ).toString('base64'),
    };
  }

  private dischargeAuthorization(input: RefundLiabilityDischarge): Record<string, string | number> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const repository = input.repository.toLowerCase();
    const message = [
      'Mizuki refund liability discharge authorization',
      'Version: 2',
      `Job: ${input.jobId}`,
      `Settlement: ${input.settlementSignature}`,
      `Repository: ${repository}`,
      `Issue: ${input.issueNumber}`,
      `Pull Request: ${input.pullRequestNumber}`,
      `Delivered Commit: ${input.deliveredCommitSha}`,
      `Reviewed Head: ${input.reviewedHeadSha}`,
      `Reviewed Base SHA: ${input.reviewedBaseSha}`,
      `Reviewed Base Ref: ${input.reviewedBaseRef}`,
      `Reviewed Diff: ${input.reviewedDiffHash}`,
      `Expires At: ${authorizationExpiresAt}`,
    ].join('\n');
    return {
      ...input,
      repository,
      authorizationExpiresAt,
      authorizationSignature: sign(
        null,
        Buffer.from(message, 'utf8'),
        this.jobAuthorityKey,
      ).toString('base64'),
    };
  }

  private escrowReleaseAuthorization(
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
  ): Record<string, string | number> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const repository = input.repository.toLowerCase();
    const reviewedAt = new Date(input.reviewedAt).toISOString();
    const message = [
      'Mizuki escrow release authorization',
      'Version: 1',
      `Escrow: ${operationId}`,
      `Repository: ${repository}`,
      `Issue: ${input.issueNumber}`,
      `Pull Request: ${input.pullRequestNumber}`,
      `Merge Commit: ${input.mergeCommitSha}`,
      `Reviewed Head: ${input.reviewedHeadSha}`,
      `Reviewed Base SHA: ${input.reviewedBaseSha}`,
      `Reviewed Base Ref: ${input.reviewedBaseRef}`,
      `Reviewed Diff: ${input.reviewedDiffHash}`,
      `Review Receipt: ${input.reviewReceiptId}`,
      `Review Receipt Hash: ${input.reviewReceiptHash}`,
      `Review Model: ${input.reviewModel}`,
      `Review Route: ${input.reviewRoute}`,
      `Reviewed At: ${reviewedAt}`,
      `Expires At: ${authorizationExpiresAt}`,
    ].join('\n');
    return {
      ...input,
      repository,
      reviewedAt,
      authorizationExpiresAt,
      authorizationSignature: sign(
        null,
        Buffer.from(message, 'utf8'),
        this.jobAuthorityKey,
      ).toString('base64'),
    };
  }

  private async wait(id: string): Promise<PolicyOperation> {
    const deadline = Date.now() + this.waitMs;
    let operation: PolicyOperation | undefined;
    while (Date.now() < deadline) {
      operation = await this.call(`/v1/operations/${id}`);
      if (operation.status === 'finalized') return operation;
      if (operation.status === 'rejected') {
        throw new PolicyRequestError(
          operation.error?.code ?? 'policy_operation_rejected',
          409,
          operation.error?.message ?? `policy operation ${id} was rejected`,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 1_000));
    }
    throw new PendingPolicyOperationError(id, operation?.status ?? 'unknown');
  }

  private async call(path: string, init: RequestInit = {}): Promise<PolicyOperation> {
    return operationSchema.parse(await this.callJson(path, init));
  }

  private async callJson(path: string, init: RequestInit = {}): Promise<unknown> {
    if (!this.config.policySignerUrl || !this.config.policySignerToken) {
      throw new Error('policy signer is not configured');
    }
    const response = await this.request(
      `${this.config.policySignerUrl.replace(/\/$/, '')}${path}`,
      {
        ...init,
        headers: {
          authorization: `Bearer ${this.config.policySignerToken}`,
          ...(init.headers ?? {}),
        },
        signal: AbortSignal.timeout(15_000),
      },
    );
    let body: unknown;
    try {
      body = (await response.json()) as unknown;
    } catch (cause) {
      if (!response.ok) {
        throw new PolicyRequestError(
          'policy_request_failed',
          response.status,
          `policy signer returned ${response.status}`,
        );
      }
      throw cause;
    }
    if (!response.ok) {
      const error = z
        .object({ error: z.object({ code: z.string(), message: z.string() }) })
        .safeParse(body);
      throw new PolicyRequestError(
        error.success ? error.data.error.code : 'policy_request_failed',
        response.status,
        error.success ? error.data.error.message : `policy signer returned ${response.status}`,
      );
    }
    return body;
  }
}

export function assertRefundCapacity(input: {
  readiness: PolicyReadiness;
  treasury: string;
  mint: string;
  decimals: number;
  escrowAuthority?: string;
  unfinishedLiabilityRaw: bigint;
  proposedPaymentRaw: bigint;
}): void {
  const { readiness } = input;
  if (!readiness.healthy || readiness.availableRefundRaw === null) {
    throw new RefundCapacityError('refund signer is not ready');
  }
  if (readiness.refundTreasury !== input.treasury) {
    throw new RefundCapacityError('refund signer treasury does not match the payment recipient');
  }
  if (readiness.refundMint !== input.mint || readiness.refundDecimals !== input.decimals) {
    throw new RefundCapacityError('refund signer asset does not match the payment asset');
  }
  if (input.escrowAuthority && readiness.escrowAuthority !== input.escrowAuthority) {
    throw new RefundCapacityError(
      'refund signer escrow authority does not match the configured escrow return recipient',
    );
  }
  if (input.unfinishedLiabilityRaw < 0n || input.proposedPaymentRaw < 0n) {
    throw new Error('refund liabilities must be non-negative');
  }
  const required = input.unfinishedLiabilityRaw + input.proposedPaymentRaw;
  if (BigInt(readiness.availableRefundRaw) < required) {
    throw new RefundCapacityError('refund capacity cannot cover existing liabilities and this job');
  }
}

export class RefundCapacityError extends Error {}

export class PendingPolicyOperationError extends Error {
  constructor(
    readonly operationId: string,
    readonly operationStatus: string,
  ) {
    super(`policy operation ${operationId} is still ${operationStatus}`);
  }
}

export class PolicyRequestError extends Error {
  readonly retryable: boolean;

  constructor(
    readonly code: string,
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = 'PolicyRequestError';
    this.retryable = status === 408 || status === 429 || status >= 500;
  }
}

function admissionBindingFromReceipt(
  receipt: RepositoryAdmissionReceipt,
): RepositoryAdmissionBinding {
  return {
    quoteId: receipt.quoteId,
    repository: receipt.repository,
    issueNumber: receipt.issueNumber,
    baseRef: receipt.baseRef,
    baseSha: receipt.baseSha,
    reservationKeyHash: receipt.reservationKeyHash,
    paymentAuthorizationHash: receipt.paymentAuthorizationHash,
  };
}

function sha256(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}
