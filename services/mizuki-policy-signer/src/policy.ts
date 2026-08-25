import { createHash, createPublicKey, randomUUID, verify } from 'node:crypto';
import { PublicKey } from '@solana/web3.js';
import { authorizedSettlementTransaction } from './chain.js';
import type { ChainCapacity, ChainGateway, UsdPriceOracle } from './chain.js';
import type {
  ChainOperation,
  BindChallenge,
  BindChallengeRequest,
  BindChallengeView,
  BindEscrowRequest,
  BindRefundLiabilityDeliveryRequest,
  CreateEscrowRequest,
  DischargeRefundLiabilityRequest,
  OperationRecord,
  PreparedTransaction,
  RefundLiability,
  RefundEscrowRequest,
  RefundRequest,
  RefundReadinessView,
  RepositoryAdmission,
  RepositoryAdmissionRequest,
  SignerReadinessEvidence,
  GitHubIdentityGrant,
  GitHubIdentityGrantRequest,
  GitHubIdentityGrantView,
  ReleaseEscrowRequest,
  RegisterRefundLiabilityRequest,
  ReconcileRepositorySettlementRequest,
  SettlementFacts,
  SettlementAuthorizationBinding,
  ValidateRepositoryAdmissionRequest,
  X402PaymentAuthorization,
} from './domain.js';
import {
  escrowAcceptanceHash,
  escrowReleaseAuthorizationMessage,
  PolicyError,
  refundAuthorizationMessage,
  refundDeliveryBindingAuthorizationMessage,
  refundDischargeAuthorizationMessage,
  requestHash,
  x402PaymentAuthorizationSchema,
} from './domain.js';
import type { MergeVerifier, RepositoryReadinessEvidence } from './github.js';
import type { SignerMetrics } from './metrics.js';
import type {
  IndependentReviewer,
  IndependentReviewRequest,
  IndependentReviewReceipt,
} from './reviewer.js';
import type { OperationStore } from './store.js';

export interface PolicyServiceConfig {
  refundTreasury: string;
  escrowAuthority: string;
  refundMint: string;
  refundDecimals: number;
  jobAuthorityPublicKey: string;
  reviewModel: string;
  refundAuthMaxTtlSeconds: number;
  operationLimitUsdCents: number;
  refundDailyLimitUsdCents: number;
  escrowDailyLimitUsdCents: number;
  maxEscrowLamports?: number;
  solFeeReserveLamports: number;
  bindChallengeTtlSeconds: number;
  claimTtlSeconds: number;
  githubGrantTtlSeconds: number;
  leaseMs?: number;
}

export class PolicyService {
  private readonly leaseMs: number;
  private readonly jobAuthorityPublicKey: PublicKey;

  constructor(
    private readonly config: PolicyServiceConfig,
    private readonly store: OperationStore,
    private readonly chain: ChainGateway,
    private readonly prices: UsdPriceOracle,
    private readonly merges: MergeVerifier,
    private readonly reviewer: IndependentReviewer,
    private readonly metrics: SignerMetrics,
    private readonly now: () => Date = () => new Date(),
  ) {
    this.leaseMs = config.leaseMs ?? 90_000;
    this.jobAuthorityPublicKey = new PublicKey(config.jobAuthorityPublicKey);
    if (
      config.jobAuthorityPublicKey === config.refundTreasury ||
      config.jobAuthorityPublicKey === config.escrowAuthority
    ) {
      throw new Error('Job authority must be distinct from both custody authorities');
    }
  }

  async registerRefundLiability(
    request: RegisterRefundLiabilityRequest,
    idempotencyKey: string,
  ): Promise<RefundLiability> {
    const immutableRequest = registrationRequestIdentity(request);
    const requestHashValue = requestHash(immutableRequest);
    const existing = await this.store.getRefundLiability(request.settlementSignature);
    if (
      existing?.idempotencyKey === idempotencyKey &&
      existing.requestHash === requestHashValue &&
      existing.jobId === request.jobId
    ) {
      const admission = await this.refundLiabilityAdmission(request);
      if (existing.repositoryAdmissionId !== admission.id) {
        throw new PolicyError(
          'refund_liability_corrupt',
          'Stored refund liability does not match its repository admission',
          503,
          true,
        );
      }
      return existing;
    }
    this.assertRefundAuthorization('register', request);
    const admission = await this.refundLiabilityAdmission(request);

    const authorization = this.settlementAuthorization(admission);
    const facts = await this.chain.readAuthorizedSettlement(
      request.settlementSignature,
      authorization,
    );
    this.validateSettlement(facts);
    this.validatePaymentWindow(facts, authorization);
    const amountUsdCents = tokenAmountUsdCents(facts.rawAmount, facts.decimals);
    this.assertOperationLimit(amountUsdCents);
    return this.store.registerRefundLiability(
      {
        id: randomUUID(),
        idempotencyKey,
        requestHash: requestHashValue,
        jobId: request.jobId,
        repositoryAdmissionId: admission.id,
        settlementSignature: facts.signature,
        repository: request.repository,
        issueNumber: request.issueNumber,
        baseRef: request.baseRef,
        baseSha: request.baseSha,
        repositoryAuthorizedAt: new Date(request.repositoryAuthorizedAt),
        authorizationEvidenceHash: request.authorizationEvidenceHash,
        reviewedHeadSha: null,
        reviewedBaseSha: null,
        reviewedBaseRef: null,
        reviewedDiffHash: null,
        deliveryBoundAt: null,
        deliveryBindingIdempotencyKey: null,
        deliveryBindingRequestHash: null,
        deliveryBindingHash: null,
        payer: facts.payer,
        treasury: facts.recipient,
        mint: facts.mint,
        rawAmount: facts.rawAmount,
        decimals: facts.decimals,
        amountUsdCents,
        settlementSlot: facts.slot,
        settlementBlockTimeUnixSeconds: facts.blockTimeUnixSeconds,
        createdAt: this.now(),
        dischargedAt: null,
        dischargeEvidenceHash: null,
        dischargeEvidence: null,
        dischargeIdempotencyKey: null,
        dischargeRequestHash: null,
      },
      await this.chain.refundCapacity(),
      this.config.refundDailyLimitUsdCents,
      this.now(),
    );
  }

  private async refundLiabilityAdmission(
    request: RegisterRefundLiabilityRequest,
  ): Promise<RepositoryAdmission> {
    const admission = await this.store.getRepositoryAdmission(request.repositoryAdmissionId);
    if (!admission) {
      throw new PolicyError(
        'repository_admission_not_found',
        'Repository admission was not found',
        404,
      );
    }
    assertRepositoryAdmissionIntegrity(admission);
    if (
      admission.evidenceHash !== request.repositoryAdmissionEvidenceHash ||
      admission.repository !== request.repository ||
      admission.issueNumber !== request.issueNumber ||
      admission.baseRef !== request.baseRef ||
      admission.baseSha !== request.baseSha
    ) {
      throw new PolicyError(
        'repository_admission_mismatch',
        'Repository admission does not match the refund liability registration',
        422,
      );
    }
    return admission;
  }

  repositoryReadiness(repository: string): Promise<RepositoryReadinessEvidence> {
    return this.merges.repositoryReadiness(repository);
  }

  async createRepositoryAdmission(
    request: RepositoryAdmissionRequest,
    idempotencyKey: string,
  ): Promise<RepositoryAdmission> {
    const parsedAuthorization = parsePaymentAuthorization(
      request.paymentAuthorization,
      request.quoteId,
    );
    this.assertPaymentRoute(parsedAuthorization);
    const settlementIdentity = authorizedSettlementTransaction({
      wireTransaction: parsedAuthorization.payload.transaction,
      feePayer: parsedAuthorization.accepted.extra.feePayer,
      rawAmount: parsedAuthorization.accepted.amount,
      notBeforeUnixSeconds: 0,
    });
    const identity = repositoryAdmissionRequestIdentity(request);
    const requestHashValue = requestHash(identity);
    const existing = await this.store.getRepositoryAdmissionByIdempotencyKey(idempotencyKey);
    if (existing) {
      assertRepositoryAdmissionIntegrity(existing);
      if (existing.requestHash !== requestHashValue) {
        throw new PolicyError(
          'idempotency_conflict',
          'Idempotency key was already used for a different request',
          409,
        );
      }
      return existing;
    }

    const readiness = await this.merges.repositoryReadiness(identity.repository);
    if (readiness.repository !== identity.repository) {
      throw new PolicyError(
        'github_repository_mismatch',
        'Verifier returned readiness for a different repository',
        503,
        true,
      );
    }
    const admittedAt = this.now();
    const tokenExpiresAt = new Date(readiness.tokenExpiresAt);
    if (!Number.isFinite(tokenExpiresAt.getTime()) || tokenExpiresAt <= admittedAt) {
      throw new PolicyError(
        'github_token_expired',
        'Verifier returned expired repository evidence',
        503,
        true,
      );
    }
    const admission = {
      id: randomUUID(),
      idempotencyKey,
      requestHash: requestHashValue,
      ...identity,
      settlementMessageHash: settlementIdentity.messageHash,
      settlementClientSignature: settlementIdentity.clientSignature,
      settlementFeePayer: settlementIdentity.feePayer,
      settlementRawAmount: parsedAuthorization.accepted.amount,
      paymentWindowStartUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) - 30,
      paymentWindowEndUnixSeconds:
        Math.floor(admittedAt.getTime() / 1_000) +
        parsedAuthorization.accepted.maxTimeoutSeconds +
        30,
      verifierAppId: readiness.verifierAppId,
      installationId: readiness.installationId,
      repositorySelection: readiness.repositorySelection,
      permissions: { ...readiness.permissions },
      tokenRepositories: readiness.tokenRepositories,
      tokenExpiresAt,
      admittedAt,
    } satisfies Omit<RepositoryAdmission, 'evidenceHash'>;
    return this.store.registerRepositoryAdmission({
      ...admission,
      evidenceHash: repositoryAdmissionEvidenceHash(admission),
    });
  }

  async validateRepositoryAdmission(
    id: string,
    request: ValidateRepositoryAdmissionRequest,
  ): Promise<RepositoryAdmission> {
    const admission = await this.store.getRepositoryAdmission(id);
    if (!admission) {
      throw new PolicyError(
        'repository_admission_not_found',
        'Repository admission was not found',
        404,
      );
    }
    assertRepositoryAdmissionIntegrity(admission);
    const { evidenceHash, ...binding } = request;
    if (
      admission.requestHash !== requestHash(repositoryAdmissionIdentity(binding)) ||
      admission.evidenceHash !== evidenceHash
    ) {
      throw new PolicyError(
        'repository_admission_mismatch',
        'Repository admission does not match the settlement reservation',
        422,
      );
    }
    return admission;
  }

  async reconcileRepositorySettlement(
    id: string,
    request: ReconcileRepositorySettlementRequest,
  ): Promise<SettlementFacts> {
    const admission = await this.store.getRepositoryAdmission(id);
    if (!admission) {
      throw new PolicyError(
        'repository_admission_not_found',
        'Repository admission was not found',
        404,
      );
    }
    assertRepositoryAdmissionIntegrity(admission);
    if (admission.evidenceHash !== request.evidenceHash) {
      throw new PolicyError(
        'repository_admission_mismatch',
        'Repository admission evidence does not match the settlement reservation',
        422,
      );
    }
    const authorization = this.settlementAuthorization(admission);
    const facts = await this.chain.reconcileSettlement(authorization);
    this.validateSettlement(facts);
    this.validatePaymentWindow(facts, authorization);
    if (facts.rawAmount !== admission.settlementRawAmount) {
      throw new PolicyError(
        'settlement_value_mismatch',
        'Finalized settlement amount does not match the payment authorization',
        422,
      );
    }
    return facts;
  }

  async bindRefundLiabilityDelivery(
    liabilityId: string,
    request: BindRefundLiabilityDeliveryRequest,
    idempotencyKey: string,
  ): Promise<RefundLiability> {
    const immutableRequest = deliveryBindingRequestIdentity(request);
    const requestHashValue = requestHash(immutableRequest);
    const liability = await this.store.getRefundLiability(request.settlementSignature);
    if (!liability || liability.id !== liabilityId || liability.jobId !== request.jobId) {
      throw new PolicyError(
        'refund_liability_not_found',
        'A matching registered refund liability was not found',
        404,
      );
    }
    if (
      liability.deliveryBoundAt &&
      liability.deliveryBindingIdempotencyKey === idempotencyKey &&
      liability.deliveryBindingRequestHash === requestHashValue
    ) {
      return liability;
    }
    if (liability.deliveryBoundAt) {
      throw new PolicyError(
        'refund_liability_delivery_bound',
        'Refund liability already has an immutable delivery binding',
        409,
      );
    }
    this.assertAuthorization(
      refundDeliveryBindingAuthorizationMessage(request),
      request.authorizationExpiresAt,
      request.authorizationSignature,
    );
    if (
      request.reviewedBaseSha !== liability.baseSha ||
      request.reviewedBaseRef !== liability.baseRef
    ) {
      throw new PolicyError(
        'refund_liability_delivery_mismatch',
        'Reviewed delivery does not match the registered base revision',
        422,
      );
    }
    await this.merges.assertCommitUnpublished(liability.repository, request.reviewedHeadSha);
    const binding = {
      reviewedHeadSha: request.reviewedHeadSha,
      reviewedBaseSha: request.reviewedBaseSha,
      reviewedBaseRef: request.reviewedBaseRef,
      reviewedDiffHash: request.reviewedDiffHash,
    };
    return this.store.bindRefundLiabilityDelivery(
      liability.id,
      idempotencyKey,
      requestHashValue,
      requestHash({
        liabilityId: liability.id,
        jobId: liability.jobId,
        settlementSignature: liability.settlementSignature,
        repository: liability.repository,
        issueNumber: liability.issueNumber,
        ...binding,
      }),
      binding,
      this.now(),
    );
  }

  async refund(request: RefundRequest, idempotencyKey: string): Promise<OperationRecord> {
    const immutableRequest = refundRequestIdentity(request);
    const clientRequestHash = requestHash(immutableRequest);
    const existing = await this.idempotentReplay(idempotencyKey, 'refund', clientRequestHash);
    if (existing) return existing;
    this.assertRefundAuthorization('execute', request);
    const liability = await this.store.getRefundLiability(request.settlementSignature);
    if (!liability || liability.jobId !== request.jobId) {
      throw new PolicyError(
        'refund_liability_not_found',
        'A matching registered refund liability was not found',
        422,
      );
    }
    const facts = await this.chain.readSettlement(request.settlementSignature);
    this.validateSettlement(facts);
    this.assertLiabilityFacts(liability, facts);
    if (BigInt(await this.chain.refundCapacity()) < BigInt(facts.rawAmount)) {
      throw new PolicyError(
        'refund_pool_insufficient',
        'Protected refund pool is insufficient',
        503,
        true,
      );
    }
    const amountUsdCents = tokenAmountUsdCents(facts.rawAmount, facts.decimals);
    this.assertOperationLimit(amountUsdCents);
    const hash = requestHash({ request: immutableRequest, facts });
    const record = await this.store.reserveRefund(
      {
        id: randomUUID(),
        idempotencyKey,
        resourceKey: `refund:${facts.signature}`,
        requestHash: hash,
        kind: 'refund',
        amountUsdCents,
        spendBucket: 'none',
        asset: facts.mint,
        recipient: facts.payer,
        details: {
          jobId: request.jobId,
          liabilityId: liability.id,
          settlementSignature: facts.signature,
          payer: facts.payer,
          mint: facts.mint,
          rawAmount: facts.rawAmount,
          decimals: facts.decimals,
          settlementSlot: facts.slot,
          clientRequestHash,
        },
      },
      liability.id,
      this.now(),
    );
    return this.drive(record.id);
  }

  async dischargeRefundLiability(
    liabilityId: string,
    request: DischargeRefundLiabilityRequest,
    idempotencyKey: string,
  ): Promise<RefundLiability> {
    const immutableRequest = dischargeRequestIdentity(request);
    const requestHashValue = requestHash(immutableRequest);
    const liability = await this.store.getRefundLiability(request.settlementSignature);
    if (!liability || liability.id !== liabilityId || liability.jobId !== request.jobId) {
      throw new PolicyError(
        'refund_liability_not_found',
        'A matching registered refund liability was not found',
        404,
      );
    }
    if (
      liability.dischargedAt &&
      liability.dischargeIdempotencyKey === idempotencyKey &&
      liability.dischargeRequestHash === requestHashValue
    ) {
      return liability;
    }
    this.assertAuthorization(
      refundDischargeAuthorizationMessage(request),
      request.authorizationExpiresAt,
      request.authorizationSignature,
    );
    if (
      request.repository !== liability.repository ||
      request.issueNumber !== liability.issueNumber ||
      !liability.deliveryBoundAt ||
      !liability.deliveryBindingHash ||
      request.deliveredCommitSha !== liability.reviewedHeadSha ||
      request.reviewedHeadSha !== liability.reviewedHeadSha ||
      request.reviewedBaseSha !== liability.reviewedBaseSha ||
      request.reviewedBaseRef !== liability.reviewedBaseRef ||
      request.reviewedDiffHash !== liability.reviewedDiffHash
    ) {
      throw new PolicyError(
        'refund_liability_delivery_mismatch',
        'Delivery evidence does not match the registered refund liability',
        422,
      );
    }
    const notBefore = new Date(
      Math.max(
        liability.settlementBlockTimeUnixSeconds * 1_000,
        liability.repositoryAuthorizedAt.getTime(),
        liability.deliveryBoundAt.getTime(),
      ),
    );
    const evidence = await this.merges.verifyRepositoryMerge({
      repository: liability.repository,
      issueNumber: liability.issueNumber,
      pullRequestNumber: request.pullRequestNumber,
      deliveredCommitSha: request.deliveredCommitSha,
      reviewedHeadSha: request.reviewedHeadSha,
      reviewedBaseSha: request.reviewedBaseSha,
      reviewedBaseRef: request.reviewedBaseRef,
      reviewedDiffHash: request.reviewedDiffHash,
      notBefore,
    });
    if (
      evidence.repository !== liability.repository ||
      evidence.issueNumber !== liability.issueNumber ||
      evidence.pullRequestNumber !== request.pullRequestNumber ||
      evidence.headCommitOid !== liability.reviewedHeadSha ||
      evidence.baseCommitOid !== liability.reviewedBaseSha ||
      evidence.baseRefName !== liability.reviewedBaseRef ||
      evidence.diffHash !== liability.reviewedDiffHash
    ) {
      throw new PolicyError(
        'github_evidence_mismatch',
        'GitHub returned delivery evidence outside the immutable liability binding',
        503,
        true,
      );
    }
    if (
      new Date(evidence.createdAt).getTime() < notBefore.getTime() ||
      new Date(evidence.mergedAt).getTime() < notBefore.getTime()
    ) {
      throw new PolicyError(
        'github_pr_too_old',
        'Pull request creation or merge predates the registered payment liability',
        422,
      );
    }
    const evidenceHash = requestHash(evidence);
    return this.store.dischargeRefundLiability(
      liability.id,
      idempotencyKey,
      requestHashValue,
      evidenceHash,
      { ...evidence },
      this.now(),
    );
  }

  async readiness(): Promise<RefundReadinessView> {
    try {
      const evidence = await this.probeReadiness();
      if (!evidence.healthy || !evidence.chain) return this.unavailableReadiness();

      const [pendingRefundRaw, rollingRefundSpendUsdCents, rollingEscrowSpendUsdCents] =
        await Promise.all([
          this.store.pendingRefundRawAmount(),
          this.store.rollingSpendUsdCents('refund', this.now()),
          this.store.rollingSpendUsdCents('escrow', this.now()),
        ]);
      const finalizedBalanceRaw = evidence.chain.refundRawAmount;
      const treasuryAvailable = BigInt(finalizedBalanceRaw) - BigInt(pendingRefundRaw);
      const remainingRefundLimitUsdCents = Math.max(
        0,
        this.config.refundDailyLimitUsdCents - rollingRefundSpendUsdCents,
      );
      const remainingEscrowLimitUsdCents = Math.max(
        0,
        this.config.escrowDailyLimitUsdCents - rollingEscrowSpendUsdCents,
      );
      const limitAvailable = rawCapacityForUsdCents(
        remainingRefundLimitUsdCents,
        this.config.refundDecimals,
      );
      const available = treasuryAvailable < limitAvailable ? treasuryAvailable : limitAvailable;
      return {
        healthy: true,
        refundTreasury: this.config.refundTreasury,
        refundMint: this.config.refundMint,
        refundDecimals: this.config.refundDecimals,
        finalizedBalanceRaw,
        pendingRefundRaw,
        treasuryAvailableRefundRaw: (treasuryAvailable > 0n ? treasuryAvailable : 0n).toString(),
        remainingRefundLimitUsdCents,
        availableRefundRaw: (available > 0n ? available : 0n).toString(),
        escrowRollingLimitUsdCents: this.config.escrowDailyLimitUsdCents,
        rollingEscrowSpendUsdCents,
        remainingEscrowLimitUsdCents,
        escrowAuthority: this.config.escrowAuthority,
        finalizedEscrowBalanceLamports: evidence.chain.escrowLamports,
        availableEscrowReserveLamports: evidence.chain.availableEscrowReserveLamports,
      };
    } catch {
      return this.unavailableReadiness();
    }
  }

  async probeReadiness(): Promise<SignerReadinessEvidence> {
    const [database, chain, prices, github, reviewer] = await Promise.allSettled([
      this.store.ping(),
      this.chain.health(),
      this.prices.solUsd(),
      this.merges.health(),
      this.reviewer.health(),
    ]);
    const priceObservations = prices.status === 'fulfilled' ? prices.value.observations : undefined;
    const priceConsensus =
      priceObservations?.length === 2 &&
      priceObservations[0]?.feed === 'primary' &&
      priceObservations[1]?.feed === 'secondary';
    const chainReady = chain.status === 'fulfilled';
    const checks = {
      database: database.status === 'fulfilled',
      rpcConsensus: chainReady,
      priceConsensus,
      githubCredential: github.status === 'fulfilled',
      independentReviewer: reviewer.status === 'fulfilled',
      escrowProgram: chainReady,
      refundCustody: chainReady,
      bountyCustody: chainReady,
    };
    const healthy = Object.values(checks).every(Boolean);

    return {
      healthy,
      observedAt: this.now().toISOString(),
      checks,
      chain:
        chain.status === 'fulfilled'
          ? {
              rpcProviders: chain.value.rpcProviders,
              escrowProgramId: chain.value.escrowProgramId,
              escrowProgramDataSha256: chain.value.escrowProgramDataSha256,
              escrowProgramImmutable: chain.value.escrowProgramImmutable,
              refundTreasury: this.config.refundTreasury,
              refundMint: this.config.refundMint,
              refundDecimals: this.config.refundDecimals,
              refundRawAmount: chain.value.refundRawAmount,
              escrowAuthority: this.config.escrowAuthority,
              escrowLamports: chain.value.escrowLamports,
              availableEscrowReserveLamports: availableEscrowReserve(
                chain.value,
                this.config.solFeeReserveLamports,
              ),
            }
          : null,
      prices:
        prices.status === 'fulfilled' && priceConsensus
          ? {
              feedCount: 2,
              priceUsdMicros: prices.value.priceUsdMicros,
              observedAt: prices.value.observedAt.toISOString(),
              observations: priceObservations.map((observation) => ({
                feed: observation.feed,
                priceUsdMicros: observation.priceUsdMicros,
                observedAt: observation.observedAt.toISOString(),
              })),
            }
          : null,
    };
  }

  private unavailableReadiness(): RefundReadinessView {
    return {
      healthy: false,
      refundTreasury: this.config.refundTreasury,
      refundMint: this.config.refundMint,
      refundDecimals: this.config.refundDecimals,
      finalizedBalanceRaw: null,
      pendingRefundRaw: null,
      treasuryAvailableRefundRaw: null,
      remainingRefundLimitUsdCents: null,
      availableRefundRaw: null,
      escrowRollingLimitUsdCents: this.config.escrowDailyLimitUsdCents,
      rollingEscrowSpendUsdCents: null,
      remainingEscrowLimitUsdCents: null,
      escrowAuthority: this.config.escrowAuthority,
      finalizedEscrowBalanceLamports: null,
      availableEscrowReserveLamports: null,
    };
  }

  async createEscrow(
    request: CreateEscrowRequest,
    idempotencyKey: string,
  ): Promise<OperationRecord> {
    const clientRequestHash = requestHash(request);
    const existing = await this.idempotentReplay(
      idempotencyKey,
      'escrow_reserve',
      clientRequestHash,
    );
    if (existing) return existing;
    const { acceptanceHash, ...acceptance } = request;
    if (
      request.reviewPolicy.model !== this.config.reviewModel ||
      escrowAcceptanceHash(acceptance) !== acceptanceHash
    ) {
      throw new PolicyError(
        'escrow_acceptance_mismatch',
        'Escrow terms do not match the committed review policy',
        422,
      );
    }
    const termsEvidence = await this.merges.verifyEscrowTerms({
      repository: request.repository,
      issueNumber: request.issueNumber,
      issueTitle: request.issueTitle,
      issueBody: request.issueBody,
      baseRef: request.baseRef,
      baseSha: request.baseSha,
    });
    if (
      termsEvidence.repository !== request.repository ||
      termsEvidence.issueNumber !== request.issueNumber ||
      termsEvidence.issueTitle !== request.issueTitle ||
      termsEvidence.issueBody !== request.issueBody ||
      termsEvidence.baseRef !== request.baseRef ||
      termsEvidence.baseSha !== request.baseSha ||
      termsEvidence.visibility !== 'PUBLIC'
    ) {
      throw new PolicyError(
        'github_escrow_terms_mismatch',
        'GitHub returned escrow terms outside the committed acceptance',
        503,
        true,
      );
    }
    this.assertOperationLimit(request.amountUsdCents);
    const expiry = new Date(request.expiresAt);
    const lifetime = expiry.getTime() - (await this.chain.unixTime()) * 1_000;
    if (lifetime < 60 * 60 * 1000 || lifetime > 8 * 24 * 60 * 60 * 1000) {
      throw new PolicyError(
        'invalid_expiry',
        'Escrow expiry must be between 1 hour and 8 days',
        422,
      );
    }
    const price = await this.prices.solUsd();
    const amountLamports = usdCentsToLamports(request.amountUsdCents, price.priceUsdMicros);
    if (amountLamports > BigInt(this.config.maxEscrowLamports ?? 1_000_000_000)) {
      throw new PolicyError(
        'escrow_asset_limit_exceeded',
        'Converted escrow amount exceeds the configured asset-unit ceiling',
        403,
      );
    }
    const capacity = await this.chain.capacity();
    const requiredLamports =
      amountLamports +
      BigInt(capacity.stateRentLamports) +
      BigInt(capacity.vaultRentLamports) +
      BigInt(capacity.guardRentLamports) +
      BigInt(this.config.solFeeReserveLamports);
    if (BigInt(capacity.escrowLamports) < requiredLamports) {
      throw new PolicyError(
        'escrow_pool_insufficient',
        'Escrow pool cannot fund this reservation',
        503,
        true,
      );
    }
    const hash = requestHash(request);
    const bountyDigest = createHash('sha256')
      .update(`mizuki:bounty:v1:${request.bountyId}`)
      .digest('hex');
    const expiresAtUnixSeconds = Math.floor(expiry.getTime() / 1_000);
    const record = await this.store.reserve(
      {
        id: randomUUID(),
        idempotencyKey,
        resourceKey: `escrow:${request.bountyId}`,
        requestHash: hash,
        kind: 'escrow_reserve',
        amountUsdCents: request.amountUsdCents,
        spendBucket: 'escrow',
        asset: 'SOL',
        recipient: 'escrow-vault',
        details: {
          bountyId: request.bountyId,
          bountyDigest,
          acceptanceHash: request.acceptanceHash,
          expiresAt: request.expiresAt,
          expiresAtUnixSeconds: String(expiresAtUnixSeconds),
          amountLamports: amountLamports.toString(),
          priceUsdMicros: price.priceUsdMicros,
          priceObservedAt: price.observedAt.toISOString(),
          ...(price.observations
            ? {
                priceObservations: price.observations.map((observation) => ({
                  feed: observation.feed,
                  priceUsdMicros: observation.priceUsdMicros,
                  observedAt: observation.observedAt.toISOString(),
                })),
              }
            : {}),
          repository: request.repository,
          issueNumber: request.issueNumber,
          issueTitle: request.issueTitle,
          issueBody: request.issueBody,
          baseRef: request.baseRef,
          baseSha: request.baseSha,
          reviewPolicy: request.reviewPolicy,
          termsEvidenceHash: requestHash(termsEvidence),
          clientRequestHash,
        },
      },
      this.config.escrowDailyLimitUsdCents,
      this.now(),
    );
    return this.drive(record.id);
  }

  async issueGitHubIdentityGrant(
    request: GitHubIdentityGrantRequest,
  ): Promise<GitHubIdentityGrantView> {
    const identity = await this.merges.verifyOauthIdentity(request.accessToken);
    const issuedAt = this.now();
    const grant: GitHubIdentityGrant = {
      id: randomUUID(),
      githubId: identity.githubId,
      login: identity.login,
      issuedAt,
      expiresAt: new Date(issuedAt.getTime() + this.config.githubGrantTtlSeconds * 1_000),
      consumedAt: null,
      challengeId: null,
    };
    const stored = await this.store.issueGitHubIdentityGrant(grant);
    return {
      id: stored.id,
      githubId: stored.githubId,
      login: stored.login,
      expiresAt: stored.expiresAt.toISOString(),
    };
  }

  async issueBindChallenge(
    escrowOperationId: string,
    request: BindChallengeRequest,
  ): Promise<BindChallengeView> {
    const escrow = await this.activeEscrow(escrowOperationId);
    await this.assertNoEscrowResolution(escrowOperationId);
    await this.assertNoEscrowBinding(escrowOperationId);
    const issuedAt = this.now();
    const grant = await this.store.getGitHubIdentityGrant(request.githubGrantId);
    if (!grant) {
      throw new PolicyError('github_grant_invalid', 'GitHub identity grant is invalid', 422);
    }
    const offerExpiresAt = new Date(requiredDetail(escrow, 'expiresAt'));
    const chainNowSeconds = await this.chain.unixTime();
    const offerRemainingMs = offerExpiresAt.getTime() - chainNowSeconds * 1_000;
    const expiresAt = new Date(
      issuedAt.getTime() + Math.min(this.config.bindChallengeTtlSeconds * 1_000, offerRemainingMs),
    );
    if (expiresAt.getTime() - issuedAt.getTime() < 60_000) {
      throw new PolicyError('escrow_offer_expiring', 'Escrow offer expires too soon to bind', 409);
    }
    const claimExpiresAt = new Date((chainNowSeconds + this.config.claimTtlSeconds) * 1_000);
    const id = randomUUID();
    const binding = {
      version: 1,
      challengeId: id,
      escrowOperationId,
      bountyId: requiredDetail(escrow, 'bountyId'),
      bountyDigest: requiredDetail(escrow, 'bountyDigest'),
      repository: requiredDetail(escrow, 'repository'),
      issueNumber: requiredNumberDetail(escrow, 'issueNumber'),
      claimantWallet: request.claimantWallet,
      claimantGitHubId: grant.githubId,
      claimantGitHubLogin: grant.login,
      claimExpiresAt: claimExpiresAt.toISOString(),
    };
    const bindingHash = requestHash(binding);
    const challenge: BindChallenge = {
      id,
      escrowOperationId,
      bindingHash,
      claimantWallet: request.claimantWallet,
      claimantGitHubId: grant.githubId,
      claimantGitHubLogin: grant.login,
      message: bindChallengeMessage(binding, bindingHash, issuedAt, expiresAt),
      claimExpiresAt,
      issuedAt,
      expiresAt,
      consumedAt: null,
      bindOperationId: null,
    };
    const stored = await this.store.issueBindChallenge(
      challenge,
      request.githubGrantId,
      this.now(),
    );
    return {
      id: stored.id,
      message: stored.message,
      expiresAt: stored.expiresAt.toISOString(),
      claimExpiresAt: stored.claimExpiresAt.toISOString(),
    };
  }

  async bindEscrow(
    escrowOperationId: string,
    request: BindEscrowRequest,
    idempotencyKey: string,
  ): Promise<OperationRecord> {
    const clientRequestHash = requestHash({ escrowOperationId, request });
    const existing = await this.idempotentReplay(idempotencyKey, 'escrow_bind', clientRequestHash);
    if (existing) return existing;
    const escrow = await this.activeEscrow(escrowOperationId);
    await this.assertNoEscrowResolution(escrowOperationId);
    await this.assertNoEscrowBinding(escrowOperationId);
    const challenge = await this.store.getBindChallenge(request.challengeId);
    if (!challenge || challenge.escrowOperationId !== escrowOperationId) {
      throw new PolicyError('challenge_invalid', 'Binding challenge is invalid', 422);
    }
    if (!verifyWalletSignature(challenge.claimantWallet, challenge.message, request.signature)) {
      throw new PolicyError('wallet_signature_invalid', 'Binding wallet signature is invalid', 422);
    }
    const record = await this.store.reserveWithBindChallenge(
      {
        id: randomUUID(),
        idempotencyKey,
        resourceKey: `escrow_binding:${escrowOperationId}`,
        requestHash: clientRequestHash,
        kind: 'escrow_bind',
        amountUsdCents: 0,
        spendBucket: 'none',
        asset: 'SOL',
        recipient: challenge.claimantWallet,
        details: {
          escrowOperationId,
          bountyDigest: requiredDetail(escrow, 'bountyDigest'),
          claimantWallet: challenge.claimantWallet,
          claimantGitHubLogin: challenge.claimantGitHubLogin,
          claimantGitHubId: challenge.claimantGitHubId,
          claimExpiresAt: challenge.claimExpiresAt.toISOString(),
          claimExpiresAtUnixSeconds: String(Math.floor(challenge.claimExpiresAt.getTime() / 1_000)),
          bindingEvidence: challenge.bindingHash,
          challengeId: challenge.id,
          clientRequestHash,
        },
      },
      challenge.id,
      challenge.bindingHash,
      this.now(),
    );
    return this.drive(record.id);
  }

  async releaseEscrow(
    escrowOperationId: string,
    request: ReleaseEscrowRequest,
    idempotencyKey: string,
  ): Promise<OperationRecord> {
    const immutableRequest = releaseEscrowRequestIdentity(request);
    const clientRequestHash = requestHash({ escrowOperationId, request: immutableRequest });
    const existing = await this.idempotentReplay(
      idempotencyKey,
      'escrow_release',
      clientRequestHash,
    );
    if (existing) return existing;
    this.assertAuthorization(
      escrowReleaseAuthorizationMessage(escrowOperationId, request),
      request.authorizationExpiresAt,
      request.authorizationSignature,
      'escrow_release',
    );
    await this.assertNoEscrowResolution(escrowOperationId, 'escrow_release');

    const escrow = await this.activeEscrow(escrowOperationId);
    const binding = await this.activeBinding(escrowOperationId);
    const reviewPolicy = requiredReviewPolicy(escrow);
    if (
      request.repository !== requiredDetail(escrow, 'repository') ||
      request.issueNumber !== requiredNumberDetail(escrow, 'issueNumber') ||
      request.reviewedBaseRef !== requiredDetail(escrow, 'baseRef') ||
      request.reviewedBaseSha !== requiredDetail(escrow, 'baseSha') ||
      request.reviewModel !== reviewPolicy.model
    ) {
      throw new PolicyError(
        'escrow_release_provenance_mismatch',
        'Release authorization does not match the escrow production revision',
        422,
      );
    }
    const claimExpiresAt = new Date(requiredDetail(binding, 'claimExpiresAt'));
    if ((await this.chain.unixTime()) * 1_000 >= claimExpiresAt.getTime()) {
      throw new PolicyError('escrow_claim_expired', 'Escrow claim has expired', 409);
    }
    const reviewedAt = new Date(request.reviewedAt);
    if (
      reviewedAt.getTime() < binding.createdAt.getTime() ||
      reviewedAt.getTime() >= claimExpiresAt.getTime()
    ) {
      throw new PolicyError(
        'escrow_review_time_invalid',
        'Independent review is outside the immutable claim window',
        422,
      );
    }
    const verified = await this.merges.verify({
      repository: request.repository,
      issueNumber: request.issueNumber,
      claimantGitHubLogin: requiredDetail(binding, 'claimantGitHubLogin'),
      pullRequestNumber: request.pullRequestNumber,
      mergeCommitSha: request.mergeCommitSha,
      reviewedHeadSha: request.reviewedHeadSha,
      reviewedBaseSha: request.reviewedBaseSha,
      reviewedBaseRef: request.reviewedBaseRef,
      reviewedDiffHash: request.reviewedDiffHash,
      expectedIssueTitle: requiredDetail(escrow, 'issueTitle'),
      expectedIssueBody: requiredDetailAllowEmpty(escrow, 'issueBody'),
      maxFiles: reviewPolicy.maxFiles,
      authorizedAt: binding.createdAt,
    });
    const { evidence, artifact } = verified;
    const mergedAt = new Date(evidence.mergedAt);
    if (mergedAt.getTime() > claimExpiresAt.getTime()) {
      throw new PolicyError(
        'github_merge_after_expiry',
        'Pull request merged after the immutable claim expiry',
        422,
      );
    }
    if (reviewedAt.getTime() > mergedAt.getTime()) {
      throw new PolicyError(
        'escrow_review_after_merge',
        'Independent review was recorded after the pull request merged',
        422,
      );
    }
    const approvedAt = new Date(evidence.approvedReviewSubmittedAt);
    if (approvedAt.getTime() < reviewedAt.getTime() || approvedAt.getTime() > mergedAt.getTime()) {
      throw new PolicyError(
        'github_maintainer_approval_time_invalid',
        'Maintainer approval is outside the reviewed pre-merge window',
        422,
      );
    }
    if (
      evidence.repository !== request.repository ||
      evidence.issueNumber !== request.issueNumber ||
      evidence.pullRequestNumber !== request.pullRequestNumber ||
      evidence.mergeCommitOid !== request.mergeCommitSha ||
      evidence.headCommitOid !== request.reviewedHeadSha ||
      evidence.baseCommitOid !== request.reviewedBaseSha ||
      evidence.baseRefName !== request.reviewedBaseRef ||
      evidence.diffHash !== request.reviewedDiffHash
    ) {
      throw new PolicyError(
        'github_evidence_mismatch',
        'GitHub returned merge evidence outside the signed release authorization',
        503,
        true,
      );
    }
    const mergeReceiptHash = requestHash(evidence);
    return this.resolveEscrow(
      escrow,
      idempotencyKey,
      'escrow_release',
      clientRequestHash,
      {
        authorization: immutableRequest,
        mergeReceiptHash,
        evidence,
        reviewAttempt: {
          status: 'reserved',
          inputHash: requestHash({
            acceptanceHash: requiredDetail(escrow, 'acceptanceHash'),
            reviewPolicyVersion: reviewPolicy.version,
            evidence,
            artifact,
          }),
          input: {
            acceptanceHash: requiredDetail(escrow, 'acceptanceHash'),
            reviewPolicyVersion: reviewPolicy.version,
            evidence,
            artifact,
          },
        },
      },
      binding,
    );
  }

  async refundEscrow(
    escrowOperationId: string,
    request: RefundEscrowRequest,
    idempotencyKey: string,
  ): Promise<OperationRecord> {
    const clientRequestHash = requestHash({ escrowOperationId, request });
    const existing = await this.idempotentReplay(
      idempotencyKey,
      'escrow_refund',
      clientRequestHash,
    );
    if (existing) return existing;
    await this.assertNoEscrowResolution(escrowOperationId, 'escrow_refund');
    const escrow = await this.activeEscrow(escrowOperationId);
    const binding = await this.store.getByResourceKey(`escrow_binding:${escrowOperationId}`);
    if (binding && binding.status !== 'finalized') {
      throw new PolicyError('escrow_binding_pending', 'Escrow binding is not finalized', 409, true);
    }
    const expiresAt = binding
      ? new Date(requiredDetail(binding, 'claimExpiresAt'))
      : new Date(requiredDetail(escrow, 'expiresAt'));
    if ((await this.chain.unixTime()) * 1_000 < expiresAt.getTime()) {
      throw new PolicyError('escrow_not_expired', 'Escrow cannot be refunded before expiry', 409);
    }
    return this.resolveEscrow(
      escrow,
      idempotencyKey,
      'escrow_refund',
      clientRequestHash,
      request,
      binding,
    );
  }

  async get(id: string): Promise<OperationRecord> {
    const record = await this.store.get(id);
    if (!record) throw new PolicyError('operation_not_found', 'Operation was not found', 404);
    return record;
  }

  async drive(id: string): Promise<OperationRecord> {
    const existing = await this.get(id);
    if (existing.status === 'finalized' || existing.status === 'rejected') return existing;

    const owner = randomUUID();
    let record = await this.store.acquireLease(id, owner, this.now(), this.leaseMs);
    if (!record) return this.get(id);
    try {
      if (record.kind === 'escrow_release') {
        record = await this.authorizeReleaseReview(record, owner);
        if (record.status === 'rejected') return record;
      }
      if (record.prepared) {
        const state = await this.chain.transactionState(record.prepared.signature);
        if (state === 'finalized') {
          return await this.store.update(record.id, owner, record.version, {
            status: 'finalized',
            transactionSignature: record.prepared.signature,
            errorCode: null,
            errorMessage: null,
          });
        }
        if (state === 'submitted') {
          return await this.store.update(record.id, owner, record.version, {
            status: 'submitted',
            transactionSignature: record.prepared.signature,
            errorCode: null,
            errorMessage: null,
          });
        }
        if (state === 'failed') {
          return await this.store.update(record.id, owner, record.version, {
            status: 'rejected',
            transactionSignature: record.prepared.signature,
            errorCode: 'transaction_failed',
            errorMessage: 'The signed transaction failed without applying its economic effect',
          });
        }
        const blockHeight = await this.chain.blockHeight();
        if (blockHeight > record.prepared.lastValidBlockHeight) {
          if (record.status !== 'prepared') {
            return await this.store.update(record.id, owner, record.version, {
              status: 'reconciling',
              transactionSignature: record.prepared.signature,
              errorCode: 'transaction_outcome_indeterminate',
              errorMessage:
                'The broadcast transaction is absent from RPC history; its economic outcome requires manual reconciliation',
            });
          }
          if (await this.releaseDeadlineElapsed(record)) {
            return await this.store.update(record.id, owner, record.version, {
              status: 'rejected',
              transactionSignature: record.prepared.signature,
              errorCode: 'release_deadline_elapsed',
              errorMessage: 'Release expired before its signed transaction was broadcast',
            });
          }
          record = await this.store.update(record.id, owner, record.version, {
            status: 'reconciling',
            prepared: null,
            transactionSignature: null,
            errorCode: 'transaction_expired',
            errorMessage: 'The unbroadcast transaction expired and will be rebuilt',
          });
        }
      }

      if (!record.prepared) {
        if (record.transactionSignature) {
          const state = await this.chain.transactionState(record.transactionSignature);
          if (state !== 'missing') {
            return await this.store.update(record.id, owner, record.version, {
              status:
                state === 'finalized' ? 'finalized' : state === 'failed' ? 'rejected' : 'submitted',
              errorCode: state === 'failed' ? 'transaction_failed' : null,
              errorMessage:
                state === 'failed'
                  ? 'The transaction failed without applying its economic effect'
                  : null,
            });
          }
          return await this.store.update(record.id, owner, record.version, {
            status: 'reconciling',
            errorCode: 'signed_transaction_missing',
            errorMessage:
              'A prior transaction signature has no durable signed payload and is absent from RPC history; manual reconciliation is required',
          });
        }
        if (await this.releaseDeadlineElapsed(record)) {
          return await this.store.update(record.id, owner, record.version, {
            status: 'rejected',
            errorCode: 'release_deadline_elapsed',
            errorMessage: 'Release is no longer valid after the immutable claim deadline',
          });
        }
        let prepared: PreparedTransaction;
        try {
          prepared = await this.chain.prepare(chainOperation(record));
        } catch (error) {
          const retryable = error instanceof PolicyError && error.retryable;
          return await this.store.update(record.id, owner, record.version, {
            status: retryable ? 'reconciling' : 'rejected',
            errorCode: error instanceof PolicyError ? error.code : 'prepare_failed',
            errorMessage: safeMessage(error),
          });
        }
        record = await this.store.update(record.id, owner, record.version, {
          status: 'prepared',
          prepared,
          transactionSignature: prepared.signature,
          details: { ...record.details, ...prepared.derived },
          errorCode: null,
          errorMessage: null,
        });
      }

      record = await this.store.update(record.id, owner, record.version, {
        status: 'broadcasting',
      });
      try {
        this.metrics.increment('broadcasts');
        await this.chain.broadcast(record.prepared!);
      } catch (error) {
        return await this.store.update(record.id, owner, record.version, {
          status: 'reconciling',
          errorCode: 'broadcast_indeterminate',
          errorMessage: safeMessage(error),
        });
      }

      const state = await this.chain.transactionState(record.prepared!.signature);
      return await this.store.update(record.id, owner, record.version, {
        status: state === 'finalized' ? 'finalized' : state === 'failed' ? 'rejected' : 'submitted',
        transactionSignature: record.prepared!.signature,
        errorCode: state === 'failed' ? 'transaction_failed' : null,
        errorMessage:
          state === 'failed' ? 'The transaction failed without applying its economic effect' : null,
      });
    } finally {
      await this.store.releaseLease(id, owner);
    }
  }

  async recover(limit = 20): Promise<void> {
    const operations = await this.store.listRecoverable(limit);
    for (const operation of operations) {
      this.metrics.increment('recoveries');
      await this.drive(operation.id).catch(() => this.metrics.increment('errors'));
    }
  }

  private async releaseDeadlineElapsed(record: OperationRecord): Promise<boolean> {
    if (record.kind !== 'escrow_release') return false;
    const claimExpiresAt = new Date(requiredDetail(record, 'claimExpiresAt'));
    return (await this.chain.unixTime()) * 1_000 >= claimExpiresAt.getTime();
  }

  private async authorizeReleaseReview(
    record: OperationRecord,
    owner: string,
  ): Promise<OperationRecord> {
    const attempt = releaseReviewAttempt(record);
    if (attempt.status === 'completed') return record;
    if (attempt.status === 'submitted') {
      return this.store.update(record.id, owner, record.version, {
        status: 'rejected',
        errorCode: 'independent_review_indeterminate',
        errorMessage:
          'Independent review was submitted without a durable receipt and will not be retried',
      });
    }

    record = await this.store.update(record.id, owner, record.version, {
      details: withReleaseReviewAttempt(record, { ...attempt, status: 'submitted' }),
    });
    let receipt: IndependentReviewReceipt;
    try {
      receipt = await this.reviewer.review(attempt.input);
    } catch (error) {
      return this.store.update(record.id, owner, record.version, {
        status: 'rejected',
        errorCode:
          error instanceof PolicyError && !error.retryable
            ? error.code
            : 'independent_review_indeterminate',
        errorMessage: safeMessage(error),
      });
    }
    if (
      receipt.inputHash !== attempt.inputHash ||
      receipt.inputHash !== requestHash(attempt.input)
    ) {
      return this.store.update(record.id, owner, record.version, {
        status: 'rejected',
        errorCode: 'independent_review_receipt_mismatch',
        errorMessage: 'Independent review receipt does not match the durable review input',
      });
    }
    return this.store.update(record.id, owner, record.version, {
      details: withReleaseReviewAttempt(record, {
        status: 'completed',
        inputHash: attempt.inputHash,
        receipt,
      }),
      errorCode: null,
      errorMessage: null,
    });
  }

  private async resolveEscrow(
    escrow: OperationRecord,
    idempotencyKey: string,
    kind: 'escrow_release' | 'escrow_refund',
    clientRequestHash: string,
    resolution: Record<string, unknown>,
    binding: OperationRecord | null,
  ): Promise<OperationRecord> {
    const escrowOperationId = escrow.id;
    const hash = requestHash({ escrowOperationId, kind, resolution });
    const claimantWallet = binding ? requiredDetail(binding, 'claimantWallet') : null;
    const record = await this.store.reserve(
      {
        id: randomUUID(),
        idempotencyKey,
        resourceKey: `escrow_resolution:${escrowOperationId}`,
        requestHash: hash,
        kind,
        amountUsdCents: 0,
        spendBucket: 'none',
        asset: 'SOL',
        recipient:
          kind === 'escrow_release'
            ? requiredResolutionClaimant(claimantWallet)
            : this.config.escrowAuthority,
        details: {
          escrowOperationId,
          bountyDigest: requiredDetail(escrow, 'bountyDigest'),
          ...(claimantWallet ? { claimantWallet } : {}),
          ...(binding ? { claimExpiresAt: requiredDetail(binding, 'claimExpiresAt') } : {}),
          resolutionEvidence: hash,
          resolution,
          clientRequestHash,
        },
      },
      this.config.escrowDailyLimitUsdCents,
      this.now(),
    );
    return this.drive(record.id);
  }

  private async activeEscrow(id: string): Promise<OperationRecord> {
    const escrow = await this.get(id);
    if (escrow.kind !== 'escrow_reserve' || escrow.status !== 'finalized') {
      throw new PolicyError('escrow_not_active', 'Escrow is not finalized and active', 409);
    }
    return escrow;
  }

  private async activeBinding(escrowOperationId: string): Promise<OperationRecord> {
    const binding = await this.store.getByResourceKey(`escrow_binding:${escrowOperationId}`);
    if (!binding || binding.kind !== 'escrow_bind' || binding.status !== 'finalized') {
      throw new PolicyError('escrow_not_bound', 'Escrow has no finalized claimant binding', 409);
    }
    return binding;
  }

  private async assertNoEscrowBinding(escrowOperationId: string): Promise<void> {
    const binding = await this.store.getByResourceKey(`escrow_binding:${escrowOperationId}`);
    if (binding && binding.status !== 'rejected') {
      throw new PolicyError('resource_conflict', 'Escrow already has a claimant binding', 409);
    }
  }

  private async assertNoEscrowResolution(
    escrowOperationId: string,
    nextKind?: 'escrow_release' | 'escrow_refund',
  ): Promise<void> {
    const resolution = await this.store.getByResourceKey(`escrow_resolution:${escrowOperationId}`);
    if (!resolution) return;
    if (
      nextKind === 'escrow_refund' &&
      resolution.kind === 'escrow_release' &&
      resolution.status === 'rejected'
    ) {
      return;
    }
    throw new PolicyError(
      'resource_conflict',
      'Escrow already has a terminal resolution operation',
      409,
    );
  }

  private async idempotentReplay(
    idempotencyKey: string,
    kind: OperationRecord['kind'],
    clientRequestHash: string,
  ): Promise<OperationRecord | null> {
    const existing = await this.store.getByIdempotencyKey(idempotencyKey);
    if (!existing) return null;
    if (existing.kind !== kind || existing.details.clientRequestHash !== clientRequestHash) {
      throw new PolicyError(
        'idempotency_conflict',
        'Idempotency key was already used for a different request',
        409,
      );
    }
    return this.drive(existing.id);
  }

  private validateSettlement(facts: SettlementFacts): void {
    if (!facts.finalized) {
      throw new PolicyError('settlement_not_finalized', 'Settlement is not finalized', 422, true);
    }
    if (!facts.succeeded) {
      throw new PolicyError('settlement_failed', 'Settlement transaction failed', 422);
    }
    if (facts.recipient !== this.config.refundTreasury) {
      throw new PolicyError('wrong_treasury', 'Settlement was not paid to this treasury', 403);
    }
    if (facts.mint !== this.config.refundMint || facts.decimals !== this.config.refundDecimals) {
      throw new PolicyError('asset_not_allowed', 'Settlement asset is not allowed', 403);
    }
    if (BigInt(facts.rawAmount) <= 0n) {
      throw new PolicyError('invalid_settlement_amount', 'Settlement amount must be positive', 422);
    }
  }

  private validatePaymentWindow(
    facts: SettlementFacts,
    authorization: SettlementAuthorizationBinding,
  ): void {
    if (
      facts.blockTimeUnixSeconds < authorization.notBeforeUnixSeconds ||
      facts.blockTimeUnixSeconds > authorization.notAfterUnixSeconds
    ) {
      throw new PolicyError(
        'settlement_outside_payment_window',
        'Settlement is outside the admitted payment window',
        422,
      );
    }
  }

  private settlementAuthorization(admission: RepositoryAdmission): SettlementAuthorizationBinding {
    return {
      messageHash: admission.settlementMessageHash,
      clientSignature: admission.settlementClientSignature,
      feePayer: admission.settlementFeePayer,
      rawAmount: admission.settlementRawAmount,
      notBeforeUnixSeconds: admission.paymentWindowStartUnixSeconds,
      notAfterUnixSeconds: admission.paymentWindowEndUnixSeconds,
    };
  }

  private assertPaymentRoute(authorization: X402PaymentAuthorization): void {
    if (
      authorization.accepted.asset !== this.config.refundMint ||
      authorization.accepted.payTo !== this.config.refundTreasury
    ) {
      throw new PolicyError(
        'payment_route_mismatch',
        'Payment authorization does not use the protected settlement route',
        422,
      );
    }
  }

  private assertRefundAuthorization(
    action: 'register' | 'execute',
    request: RefundRequest | RegisterRefundLiabilityRequest,
  ): void {
    this.assertAuthorization(
      refundAuthorizationMessage(action, request),
      request.authorizationExpiresAt,
      request.authorizationSignature,
    );
  }

  private assertAuthorization(
    message: string,
    authorizationExpiresAt: string,
    authorizationSignature: string,
    kind: 'refund' | 'escrow_release' = 'refund',
  ): void {
    const label = kind === 'refund' ? 'Refund' : 'Escrow release';
    const now = this.now().getTime();
    const expiresAt = new Date(authorizationExpiresAt).getTime();
    if (expiresAt <= now) {
      throw new PolicyError(
        `${kind}_authorization_expired`,
        `${label} authorization has expired`,
        401,
      );
    }
    if (expiresAt - now > this.config.refundAuthMaxTtlSeconds * 1_000) {
      throw new PolicyError(
        `${kind}_authorization_ttl_invalid`,
        `${label} authorization lifetime exceeds policy`,
        401,
      );
    }
    if (!verifyEd25519Signature(this.jobAuthorityPublicKey, message, authorizationSignature)) {
      throw new PolicyError(
        `${kind}_authorization_invalid`,
        `${label} authorization signature is invalid`,
        401,
      );
    }
  }

  private assertLiabilityFacts(liability: RefundLiability, facts: SettlementFacts): void {
    if (
      liability.settlementSignature !== facts.signature ||
      liability.payer !== facts.payer ||
      liability.treasury !== facts.recipient ||
      liability.mint !== facts.mint ||
      liability.rawAmount !== facts.rawAmount ||
      liability.decimals !== facts.decimals ||
      liability.settlementSlot !== facts.slot ||
      liability.settlementBlockTimeUnixSeconds !== facts.blockTimeUnixSeconds
    ) {
      throw new PolicyError(
        'refund_liability_mismatch',
        'Finalized settlement no longer matches the registered refund liability',
        503,
        true,
      );
    }
  }

  private assertOperationLimit(amountUsdCents: number): void {
    if (amountUsdCents > this.config.operationLimitUsdCents) {
      throw new PolicyError(
        'operation_limit_exceeded',
        'Per-operation spending limit exceeded',
        403,
      );
    }
  }
}

function tokenAmountUsdCents(rawAmount: string, decimals: number): number {
  const scale = 10n ** BigInt(decimals);
  const numerator = BigInt(rawAmount) * 100n;
  const cents = (numerator + scale - 1n) / scale;
  if (cents > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new PolicyError('invalid_settlement_amount', 'Settlement amount is too large', 422);
  }
  return Number(cents);
}

function refundRequestIdentity(
  request: Pick<RefundRequest, 'jobId' | 'settlementSignature'>,
): Pick<RefundRequest, 'jobId' | 'settlementSignature'> {
  return { jobId: request.jobId, settlementSignature: request.settlementSignature };
}

type RepositoryAdmissionIdentity = Pick<
  RepositoryAdmission,
  | 'quoteId'
  | 'repository'
  | 'issueNumber'
  | 'baseRef'
  | 'baseSha'
  | 'reservationKeyHash'
  | 'paymentAuthorizationHash'
>;

function repositoryAdmissionRequestIdentity(
  request: RepositoryAdmissionRequest,
): RepositoryAdmissionIdentity {
  return repositoryAdmissionIdentity({
    ...request,
    paymentAuthorizationHash: sha256(request.paymentAuthorization),
  });
}

function repositoryAdmissionIdentity(
  request: RepositoryAdmissionIdentity,
): RepositoryAdmissionIdentity {
  return {
    quoteId: request.quoteId,
    repository: request.repository.toLowerCase(),
    issueNumber: request.issueNumber,
    baseRef: request.baseRef,
    baseSha: request.baseSha,
    reservationKeyHash: request.reservationKeyHash,
    paymentAuthorizationHash: request.paymentAuthorizationHash,
  };
}

function repositoryAdmissionEvidenceHash(
  admission: Omit<RepositoryAdmission, 'evidenceHash'>,
): string {
  return requestHash({
    version: 1,
    ...repositoryAdmissionIdentity(admission),
    settlementMessageHash: admission.settlementMessageHash,
    settlementClientSignature: admission.settlementClientSignature,
    settlementFeePayer: admission.settlementFeePayer,
    settlementRawAmount: admission.settlementRawAmount,
    paymentWindowStartUnixSeconds: admission.paymentWindowStartUnixSeconds,
    paymentWindowEndUnixSeconds: admission.paymentWindowEndUnixSeconds,
    verifierAppId: admission.verifierAppId,
    installationId: admission.installationId,
    repositorySelection: admission.repositorySelection,
    permissions: admission.permissions,
    tokenRepositories: admission.tokenRepositories,
    tokenExpiresAt: admission.tokenExpiresAt.toISOString(),
    admittedAt: admission.admittedAt.toISOString(),
  });
}

function assertRepositoryAdmissionIntegrity(admission: RepositoryAdmission): void {
  if (repositoryAdmissionEvidenceHash(admission) !== admission.evidenceHash) {
    throw new PolicyError(
      'repository_admission_corrupt',
      'Stored repository admission failed its integrity check',
      503,
      true,
    );
  }
}

function parsePaymentAuthorization(value: string, quoteId: string): X402PaymentAuthorization {
  let parsed: unknown;
  try {
    const encoded = Buffer.from(value, 'base64');
    if (encoded.toString('base64') !== value) throw new Error('non-canonical base64');
    const json = new TextDecoder('utf-8', { fatal: true }).decode(encoded);
    parsed = JSON.parse(json);
  } catch {
    throw new PolicyError(
      'payment_authorization_invalid',
      'Payment authorization is not canonical base64 JSON',
      422,
    );
  }

  const authorization = x402PaymentAuthorizationSchema.safeParse(parsed);
  if (!authorization.success) {
    throw new PolicyError(
      'payment_authorization_invalid',
      'Payment authorization does not use the supported exact SVM format',
      422,
    );
  }

  const resource = new URL(authorization.data.resource.url);
  const parameters = [...resource.searchParams.keys()];
  if (
    resource.protocol !== 'https:' ||
    resource.username ||
    resource.password ||
    resource.hash ||
    resource.pathname !== '/v1/jobs' ||
    parameters.length !== 1 ||
    parameters[0] !== 'quote_id' ||
    resource.searchParams.get('quote_id') !== quoteId
  ) {
    throw new PolicyError(
      'payment_resource_mismatch',
      'Payment authorization resource does not match the admitted quote',
      422,
    );
  }
  return authorization.data;
}

function sha256(value: string): string {
  return createHash('sha256').update(value).digest('hex');
}

function registrationRequestIdentity(
  request: RegisterRefundLiabilityRequest,
): Omit<RegisterRefundLiabilityRequest, 'authorizationExpiresAt' | 'authorizationSignature'> {
  return {
    jobId: request.jobId,
    settlementSignature: request.settlementSignature,
    repositoryAdmissionId: request.repositoryAdmissionId,
    repositoryAdmissionEvidenceHash: request.repositoryAdmissionEvidenceHash,
    repository: request.repository,
    issueNumber: request.issueNumber,
    baseRef: request.baseRef,
    baseSha: request.baseSha,
    repositoryAuthorizedAt: request.repositoryAuthorizedAt,
    authorizationEvidenceHash: request.authorizationEvidenceHash,
  };
}

function deliveryBindingRequestIdentity(
  request: BindRefundLiabilityDeliveryRequest,
): Omit<BindRefundLiabilityDeliveryRequest, 'authorizationExpiresAt' | 'authorizationSignature'> {
  return {
    jobId: request.jobId,
    settlementSignature: request.settlementSignature,
    reviewedHeadSha: request.reviewedHeadSha,
    reviewedBaseSha: request.reviewedBaseSha,
    reviewedBaseRef: request.reviewedBaseRef,
    reviewedDiffHash: request.reviewedDiffHash,
  };
}

function dischargeRequestIdentity(
  request: DischargeRefundLiabilityRequest,
): Pick<
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
> {
  return {
    jobId: request.jobId,
    settlementSignature: request.settlementSignature,
    repository: request.repository,
    issueNumber: request.issueNumber,
    pullRequestNumber: request.pullRequestNumber,
    deliveredCommitSha: request.deliveredCommitSha,
    reviewedHeadSha: request.reviewedHeadSha,
    reviewedBaseSha: request.reviewedBaseSha,
    reviewedBaseRef: request.reviewedBaseRef,
    reviewedDiffHash: request.reviewedDiffHash,
  };
}

function releaseEscrowRequestIdentity(
  request: ReleaseEscrowRequest,
): Omit<ReleaseEscrowRequest, 'authorizationExpiresAt' | 'authorizationSignature'> {
  return {
    repository: request.repository,
    issueNumber: request.issueNumber,
    pullRequestNumber: request.pullRequestNumber,
    mergeCommitSha: request.mergeCommitSha,
    reviewedHeadSha: request.reviewedHeadSha,
    reviewedBaseSha: request.reviewedBaseSha,
    reviewedBaseRef: request.reviewedBaseRef,
    reviewedDiffHash: request.reviewedDiffHash,
    reviewReceiptId: request.reviewReceiptId,
    reviewReceiptHash: request.reviewReceiptHash,
    reviewModel: request.reviewModel,
    reviewRoute: request.reviewRoute,
    reviewedAt: request.reviewedAt,
  };
}

function rawCapacityForUsdCents(usdCents: number, decimals: number): bigint {
  return (BigInt(usdCents) * 10n ** BigInt(decimals)) / 100n;
}

function usdCentsToLamports(amountUsdCents: number, priceUsdMicros: number): bigint {
  const numerator = BigInt(amountUsdCents) * 10_000_000_000_000n;
  const denominator = BigInt(priceUsdMicros);
  const lamports = (numerator + denominator - 1n) / denominator;
  if (lamports <= 0n) {
    throw new PolicyError(
      'invalid_escrow_amount',
      'Converted escrow amount is outside safety bounds',
      422,
    );
  }
  return lamports;
}

function availableEscrowReserve(capacity: ChainCapacity, feeReserveLamports: number): string {
  const overhead =
    BigInt(capacity.stateRentLamports) +
    BigInt(capacity.vaultRentLamports) +
    BigInt(capacity.guardRentLamports) +
    BigInt(feeReserveLamports);
  const available = BigInt(capacity.escrowLamports) - overhead;
  return (available > 0n ? available : 0n).toString();
}

function chainOperation(record: OperationRecord): ChainOperation {
  if (record.kind === 'refund') {
    return {
      kind: 'refund',
      intentId: record.id,
      payer: requiredDetail(record, 'payer'),
      mint: requiredDetail(record, 'mint'),
      rawAmount: requiredDetail(record, 'rawAmount'),
      decimals: requiredNumberDetail(record, 'decimals'),
    };
  }
  if (record.kind === 'escrow_reserve') {
    return {
      kind: 'escrow_reserve',
      intentId: record.id,
      bountyDigest: requiredDetail(record, 'bountyDigest'),
      amountLamports: requiredDetail(record, 'amountLamports'),
      expiresAtUnixSeconds: requiredDetail(record, 'expiresAtUnixSeconds'),
      acceptanceHash: requiredDetail(record, 'acceptanceHash'),
    };
  }
  if (record.kind === 'escrow_bind') {
    return {
      kind: 'escrow_bind',
      intentId: record.id,
      bountyDigest: requiredDetail(record, 'bountyDigest'),
      claimantWallet: requiredDetail(record, 'claimantWallet'),
      claimExpiresAtUnixSeconds: requiredDetail(record, 'claimExpiresAtUnixSeconds'),
      bindingEvidence: requiredDetail(record, 'bindingEvidence'),
    };
  }
  return {
    kind: record.kind,
    intentId: record.id,
    bountyDigest: requiredDetail(record, 'bountyDigest'),
    ...(record.kind === 'escrow_release'
      ? { claimantWallet: requiredDetail(record, 'claimantWallet') }
      : {}),
    resolutionEvidence: requiredDetail(record, 'resolutionEvidence'),
  };
}

function bindChallengeMessage(
  binding: {
    version: number;
    challengeId: string;
    escrowOperationId: string;
    bountyId: string;
    repository: string;
    issueNumber: number;
    claimantWallet: string;
    claimantGitHubId: string;
    claimantGitHubLogin: string;
    claimExpiresAt: string;
  },
  bindingHash: string,
  issuedAt: Date,
  expiresAt: Date,
): string {
  return [
    'Mizuki contributor binding',
    'Version: 1',
    `Challenge: ${binding.challengeId}`,
    `Reservation: ${binding.escrowOperationId}`,
    `Bounty: ${binding.bountyId}`,
    `Repository: ${binding.repository}`,
    `Issue: ${binding.issueNumber}`,
    `GitHub: ${binding.claimantGitHubLogin}`,
    `GitHub ID: ${binding.claimantGitHubId}`,
    `Wallet: ${binding.claimantWallet}`,
    `Claim Expires At: ${binding.claimExpiresAt}`,
    `Issued At: ${issuedAt.toISOString()}`,
    `Challenge Expires At: ${expiresAt.toISOString()}`,
    `Commitment: ${bindingHash}`,
  ].join('\n');
}

function verifyWalletSignature(wallet: string, message: string, signature: string): boolean {
  try {
    return verifyEd25519Signature(new PublicKey(wallet), message, signature);
  } catch {
    return false;
  }
}

function verifyEd25519Signature(publicKey: PublicKey, message: string, signature: string): boolean {
  try {
    const spki = Buffer.concat([
      Buffer.from('302a300506032b6570032100', 'hex'),
      Buffer.from(publicKey.toBytes()),
    ]);
    return verify(
      null,
      Buffer.from(message, 'utf8'),
      createPublicKey({ key: spki, format: 'der', type: 'spki' }),
      Buffer.from(signature, 'base64'),
    );
  } catch {
    return false;
  }
}

function requiredResolutionClaimant(value: string | null): string {
  if (!value) throw new Error('Release operation is missing a bound claimant');
  return value;
}

function requiredDetail(record: OperationRecord, key: string): string {
  const value = record.details[key];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`Operation ${record.id} is missing ${key}`);
  }
  return value;
}

function requiredDetailAllowEmpty(record: OperationRecord, key: string): string {
  const value = record.details[key];
  if (typeof value !== 'string') throw new Error(`Operation ${record.id} is missing ${key}`);
  return value;
}

function requiredReviewPolicy(record: OperationRecord): {
  version: 1;
  model: string;
  maxFiles: number;
} {
  const value = record.details.reviewPolicy;
  if (
    !value ||
    typeof value !== 'object' ||
    Array.isArray(value) ||
    (value as Record<string, unknown>).version !== 1 ||
    typeof (value as Record<string, unknown>).model !== 'string' ||
    !Number.isSafeInteger((value as Record<string, unknown>).maxFiles) ||
    Number((value as Record<string, unknown>).maxFiles) < 1 ||
    Number((value as Record<string, unknown>).maxFiles) > 20
  ) {
    throw new Error(`Operation ${record.id} has an invalid review policy`);
  }
  return value as { version: 1; model: string; maxFiles: number };
}

type ReleaseReviewAttempt =
  | {
      status: 'reserved' | 'submitted';
      inputHash: string;
      input: IndependentReviewRequest;
    }
  | {
      status: 'completed';
      inputHash: string;
      receipt: IndependentReviewReceipt;
    };

function releaseReviewAttempt(record: OperationRecord): ReleaseReviewAttempt {
  const resolution = objectValue(record.details.resolution);
  const attempt = objectValue(resolution.reviewAttempt);
  const status = attempt.status;
  const inputHash = attempt.inputHash;
  if (typeof inputHash !== 'string' || !/^[a-f0-9]{64}$/.test(inputHash)) {
    throw new Error(`Operation ${record.id} has an invalid review input hash`);
  }
  if (status === 'completed') {
    const receipt = objectValue(attempt.receipt) as unknown as IndependentReviewReceipt;
    if (receipt.approved !== true || receipt.inputHash !== inputHash) {
      throw new Error(`Operation ${record.id} has an invalid independent review receipt`);
    }
    return { status, inputHash, receipt };
  }
  if (status !== 'reserved' && status !== 'submitted') {
    throw new Error(`Operation ${record.id} has an invalid independent review state`);
  }
  const input = objectValue(attempt.input) as unknown as IndependentReviewRequest;
  if (requestHash(input) !== inputHash) {
    throw new Error(`Operation ${record.id} independent review input failed integrity check`);
  }
  return { status, inputHash, input };
}

function withReleaseReviewAttempt(
  record: OperationRecord,
  attempt: ReleaseReviewAttempt,
): Record<string, unknown> {
  const resolution = objectValue(record.details.resolution);
  return {
    ...record.details,
    resolution: { ...resolution, reviewAttempt: attempt },
  };
}

function objectValue(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('Operation contains malformed structured evidence');
  }
  return value as Record<string, unknown>;
}

function requiredNumberDetail(record: OperationRecord, key: string): number {
  const value = record.details[key];
  if (typeof value !== 'number' || !Number.isInteger(value)) {
    throw new Error(`Operation ${record.id} is missing ${key}`);
  }
  return value;
}

function safeMessage(error: unknown): string {
  if (!(error instanceof Error)) return 'Transaction broadcast result is indeterminate';
  return error.message.slice(0, 240);
}
