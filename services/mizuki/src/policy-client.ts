import { createPrivateKey, sign, type KeyObject } from 'node:crypto';
import { z } from 'zod';
import type { Config } from './config.js';

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

const refundLiabilitySchema = z.object({
  id: z.string().uuid(),
  jobId: z.string().min(1),
  settlementSignature: z.string().min(1),
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

export interface RefundCapacityPolicy {
  readiness(): Promise<PolicyReadiness>;
}

export interface PaymentPolicy extends RefundCapacityPolicy {
  registerRefundLiability(jobId: string, settlementSignature: string): Promise<RefundLiability>;
  dischargeRefundLiability(
    liabilityId: string,
    input: {
      jobId: string;
      settlementSignature: string;
      repository: string;
      pullRequestNumber: number;
    },
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
  releaseEscrow(operationId: string, pullRequestNumber: number): Promise<PolicyOperation>;
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
      'policySignerUrl' | 'policySignerToken' | 'jobAuthoritySeed'
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
  ): Promise<RefundLiability> {
    const body = this.refundAuthorization('register', jobId, settlementSignature);
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
    await this.registerRefundLiability(jobId, settlementSignature);
    return this.mutate(
      '/v1/refunds',
      `mizuki-refund-${jobId}`,
      this.refundAuthorization('execute', jobId, settlementSignature),
    );
  }

  async dischargeRefundLiability(
    liabilityId: string,
    input: {
      jobId: string;
      settlementSignature: string;
      repository: string;
      pullRequestNumber: number;
    },
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

  async releaseEscrow(operationId: string, pullRequestNumber: number): Promise<PolicyOperation> {
    return this.mutate(
      `/v1/escrows/${operationId}/release`,
      `mizuki-escrow-release-${operationId}`,
      { pullRequestNumber },
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

  private refundAuthorization(
    action: 'register' | 'execute',
    jobId: string,
    settlementSignature: string,
  ): Record<string, string> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const title =
      action === 'register'
        ? 'Mizuki refund liability registration'
        : 'Mizuki refund execution authorization';
    const message = [
      title,
      'Version: 1',
      `Job: ${jobId}`,
      `Settlement: ${settlementSignature}`,
      `Expires At: ${authorizationExpiresAt}`,
    ].join('\n');
    return {
      jobId,
      settlementSignature,
      authorizationExpiresAt,
      authorizationSignature: sign(
        null,
        Buffer.from(message, 'utf8'),
        this.jobAuthorityKey,
      ).toString('base64'),
    };
  }

  private dischargeAuthorization(input: {
    jobId: string;
    settlementSignature: string;
    repository: string;
    pullRequestNumber: number;
  }): Record<string, string | number> {
    if (!this.jobAuthorityKey) throw new Error('job authority is not configured');
    const authorizationExpiresAt = new Date(this.now().getTime() + 5 * 60_000).toISOString();
    const repository = input.repository.toLowerCase();
    const message = [
      'Mizuki refund liability discharge authorization',
      'Version: 1',
      `Job: ${input.jobId}`,
      `Settlement: ${input.settlementSignature}`,
      `Repository: ${repository}`,
      `Pull Request: ${input.pullRequestNumber}`,
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
      'refund signer escrow authority does not match the capability payout wallet',
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
