import type {
  BindChallenge,
  GitHubIdentityGrant,
  OperationRecord,
  OperationStatus,
  PreparedTransaction,
  RefundLiability,
  RepositoryAdmission,
  ReserveOperation,
} from './domain.js';
import { PolicyError } from './domain.js';

export interface OperationPatch {
  status?: OperationStatus;
  prepared?: PreparedTransaction | null;
  transactionSignature?: string | null;
  errorCode?: string | null;
  errorMessage?: string | null;
  details?: Record<string, unknown>;
}

export interface StoreStats {
  total: number;
  byStatus: Partial<Record<OperationStatus, number>>;
}

export interface RefundLiabilityDeliveryBinding {
  reviewedHeadSha: string;
  reviewedBaseSha: string;
  reviewedBaseRef: string;
  reviewedDiffHash: string;
}

export interface OperationStore {
  migrate(): Promise<void>;
  registerRepositoryAdmission(admission: RepositoryAdmission): Promise<RepositoryAdmission>;
  getRepositoryAdmission(id: string): Promise<RepositoryAdmission | null>;
  getRepositoryAdmissionByIdempotencyKey(key: string): Promise<RepositoryAdmission | null>;
  registerRefundLiability(
    liability: RefundLiability,
    maxOutstandingRaw: string,
    dailyLimitUsdCents: number,
    now: Date,
  ): Promise<RefundLiability>;
  getRefundLiability(settlementSignature: string): Promise<RefundLiability | null>;
  bindRefundLiabilityDelivery(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    bindingHash: string,
    binding: RefundLiabilityDeliveryBinding,
    now: Date,
  ): Promise<RefundLiability>;
  dischargeRefundLiability(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    evidenceHash: string,
    evidence: Record<string, unknown>,
    now: Date,
  ): Promise<RefundLiability>;
  reserveRefund(input: ReserveOperation, liabilityId: string, now: Date): Promise<OperationRecord>;
  reserve(input: ReserveOperation, dailyLimitUsdCents: number, now: Date): Promise<OperationRecord>;
  issueGitHubIdentityGrant(grant: GitHubIdentityGrant): Promise<GitHubIdentityGrant>;
  getGitHubIdentityGrant(id: string): Promise<GitHubIdentityGrant | null>;
  issueBindChallenge(challenge: BindChallenge, grantId: string, now: Date): Promise<BindChallenge>;
  getBindChallenge(id: string): Promise<BindChallenge | null>;
  reserveWithBindChallenge(
    input: ReserveOperation,
    challengeId: string,
    bindingHash: string,
    now: Date,
  ): Promise<OperationRecord>;
  get(id: string): Promise<OperationRecord | null>;
  getByIdempotencyKey(key: string): Promise<OperationRecord | null>;
  getByResourceKey(key: string): Promise<OperationRecord | null>;
  acquireLease(
    id: string,
    owner: string,
    now: Date,
    leaseMs: number,
  ): Promise<OperationRecord | null>;
  update(
    id: string,
    owner: string,
    expectedVersion: number,
    patch: OperationPatch,
  ): Promise<OperationRecord>;
  releaseLease(id: string, owner: string): Promise<void>;
  listRecoverable(limit: number): Promise<OperationRecord[]>;
  stats(): Promise<StoreStats>;
  pendingRefundRawAmount(): Promise<string>;
  rollingSpendUsdCents(bucket: 'refund' | 'escrow', now: Date): Promise<number>;
  ping(): Promise<void>;
  close(): Promise<void>;
}

export class InMemoryOperationStore implements OperationStore {
  private readonly records = new Map<string, OperationRecord>();
  private readonly byIdempotency = new Map<string, string>();
  private readonly byResource = new Map<string, string>();
  private readonly bindChallenges = new Map<string, BindChallenge>();
  private readonly githubIdentityGrants = new Map<string, GitHubIdentityGrant>();
  private readonly repositoryAdmissions = new Map<string, RepositoryAdmission>();
  private readonly admissionByIdempotency = new Map<string, string>();
  private readonly admissionByQuote = new Map<string, string>();
  private readonly admissionByReservation = new Map<string, string>();
  private readonly admissionByPayment = new Map<string, string>();
  private readonly refundLiabilities = new Map<string, RefundLiability>();
  private readonly liabilityByIdempotency = new Map<string, string>();
  private readonly liabilityByJob = new Map<string, string>();
  private readonly liabilityDeliveryByIdempotency = new Map<string, string>();
  private readonly liabilityDischargeByIdempotency = new Map<string, string>();
  private tail: Promise<void> = Promise.resolve();

  async migrate(): Promise<void> {}

  async registerRepositoryAdmission(admission: RepositoryAdmission): Promise<RepositoryAdmission> {
    return this.exclusive(() => {
      const idempotentId = this.admissionByIdempotency.get(admission.idempotencyKey);
      if (idempotentId) {
        const existing = this.repositoryAdmissions.get(idempotentId)!;
        if (existing.requestHash !== admission.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        return cloneAdmission(existing);
      }
      if (
        this.admissionByQuote.has(admission.quoteId) ||
        this.admissionByReservation.has(admission.reservationKeyHash) ||
        this.admissionByPayment.has(admission.paymentAuthorizationHash)
      ) {
        throw new PolicyError(
          'repository_admission_conflict',
          'Quote, reservation, or payment proof already has a different admission',
          409,
        );
      }
      const stored = cloneAdmission(admission);
      this.repositoryAdmissions.set(stored.id, stored);
      this.admissionByIdempotency.set(stored.idempotencyKey, stored.id);
      this.admissionByQuote.set(stored.quoteId, stored.id);
      this.admissionByReservation.set(stored.reservationKeyHash, stored.id);
      this.admissionByPayment.set(stored.paymentAuthorizationHash, stored.id);
      return cloneAdmission(stored);
    });
  }

  async getRepositoryAdmission(id: string): Promise<RepositoryAdmission | null> {
    return this.exclusive(() => {
      const admission = this.repositoryAdmissions.get(id);
      return admission ? cloneAdmission(admission) : null;
    });
  }

  async getRepositoryAdmissionByIdempotencyKey(key: string): Promise<RepositoryAdmission | null> {
    return this.exclusive(() => {
      const id = this.admissionByIdempotency.get(key);
      const admission = id ? this.repositoryAdmissions.get(id) : undefined;
      return admission ? cloneAdmission(admission) : null;
    });
  }

  async registerRefundLiability(
    liability: RefundLiability,
    maxOutstandingRaw: string,
    dailyLimitUsdCents: number,
    now: Date,
  ): Promise<RefundLiability> {
    return this.exclusive(() => {
      const idempotentSignature = this.liabilityByIdempotency.get(liability.idempotencyKey);
      if (idempotentSignature) {
        const existing = this.refundLiabilities.get(idempotentSignature)!;
        if (existing.requestHash !== liability.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        return cloneLiability(existing);
      }
      if (this.refundLiabilities.has(liability.settlementSignature)) {
        throw new PolicyError(
          'settlement_liability_conflict',
          'Settlement is already registered to a refund liability',
          409,
        );
      }
      if (this.liabilityByJob.has(liability.jobId)) {
        throw new PolicyError(
          'job_liability_conflict',
          'Job is already registered to a refund liability',
          409,
        );
      }
      const cutoff = now.getTime() - 24 * 60 * 60 * 1_000;
      const rollingSpend = [...this.refundLiabilities.values()]
        .filter((entry) => entry.createdAt.getTime() >= cutoff)
        .reduce((total, entry) => total + entry.amountUsdCents, 0);
      if (rollingSpend + liability.amountUsdCents > dailyLimitUsdCents) {
        throw new PolicyError(
          'daily_limit_exceeded',
          'Rolling 24-hour refund liability limit exceeded',
          429,
          true,
        );
      }
      const outstanding = [...this.refundLiabilities.values()]
        .filter((entry) => this.isLiabilityOutstanding(entry))
        .reduce((total, entry) => total + BigInt(entry.rawAmount), 0n);
      if (outstanding + BigInt(liability.rawAmount) > BigInt(maxOutstandingRaw)) {
        throw new PolicyError(
          'refund_pool_insufficient',
          'Protected refund pool cannot cover all registered liabilities',
          503,
          true,
        );
      }
      const stored = cloneLiability({ ...liability, createdAt: new Date(now) });
      this.refundLiabilities.set(stored.settlementSignature, stored);
      this.liabilityByIdempotency.set(stored.idempotencyKey, stored.settlementSignature);
      this.liabilityByJob.set(stored.jobId, stored.settlementSignature);
      return cloneLiability(stored);
    });
  }

  async getRefundLiability(settlementSignature: string): Promise<RefundLiability | null> {
    return this.exclusive(() => {
      const liability = this.refundLiabilities.get(settlementSignature);
      return liability ? cloneLiability(liability) : null;
    });
  }

  async bindRefundLiabilityDelivery(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    bindingHash: string,
    binding: RefundLiabilityDeliveryBinding,
    now: Date,
  ): Promise<RefundLiability> {
    return this.exclusive(() => {
      const liability = [...this.refundLiabilities.values()].find(
        (entry) => entry.id === liabilityId,
      );
      if (!liability) {
        throw new PolicyError('refund_liability_not_found', 'Refund liability was not found', 404);
      }
      const idempotentLiabilityId = this.liabilityDeliveryByIdempotency.get(idempotencyKey);
      if (idempotentLiabilityId && idempotentLiabilityId !== liabilityId) {
        throw new PolicyError(
          'idempotency_conflict',
          'Idempotency key was already used for a different request',
          409,
        );
      }
      if (liability.deliveryBoundAt) {
        if (
          liability.deliveryBindingIdempotencyKey === idempotencyKey &&
          liability.deliveryBindingRequestHash === requestHash
        ) {
          return cloneLiability(liability);
        }
        throw new PolicyError(
          'refund_liability_delivery_bound',
          'Refund liability already has an immutable delivery binding',
          409,
        );
      }
      if (liability.dischargedAt) {
        throw new PolicyError(
          'refund_liability_discharged',
          'Discharged refund liability cannot be rebound',
          409,
        );
      }
      const refund = this.lookup(this.byResource.get(`refund:${liability.settlementSignature}`));
      if (refund && refund.status !== 'rejected') {
        throw new PolicyError(
          'refund_already_started',
          'Refund liability cannot be bound after refund execution starts',
          409,
        );
      }
      liability.reviewedHeadSha = binding.reviewedHeadSha;
      liability.reviewedBaseSha = binding.reviewedBaseSha;
      liability.reviewedBaseRef = binding.reviewedBaseRef;
      liability.reviewedDiffHash = binding.reviewedDiffHash;
      liability.deliveryBoundAt = new Date(Math.floor(now.getTime() / 1_000) * 1_000);
      liability.deliveryBindingIdempotencyKey = idempotencyKey;
      liability.deliveryBindingRequestHash = requestHash;
      liability.deliveryBindingHash = bindingHash;
      this.liabilityDeliveryByIdempotency.set(idempotencyKey, liability.id);
      return cloneLiability(liability);
    });
  }

  async dischargeRefundLiability(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    evidenceHash: string,
    evidence: Record<string, unknown>,
    now: Date,
  ): Promise<RefundLiability> {
    return this.exclusive(() => {
      const liability = [...this.refundLiabilities.values()].find(
        (entry) => entry.id === liabilityId,
      );
      if (!liability) {
        throw new PolicyError('refund_liability_not_found', 'Refund liability was not found', 404);
      }
      const idempotentLiabilityId = this.liabilityDischargeByIdempotency.get(idempotencyKey);
      if (idempotentLiabilityId && idempotentLiabilityId !== liabilityId) {
        throw new PolicyError(
          'idempotency_conflict',
          'Idempotency key was already used for a different request',
          409,
        );
      }
      if (liability.dischargedAt) {
        if (
          liability.dischargeIdempotencyKey === idempotencyKey &&
          liability.dischargeRequestHash === requestHash
        ) {
          return cloneLiability(liability);
        }
        throw new PolicyError(
          'refund_liability_discharged',
          'Refund liability is already discharged',
          409,
        );
      }
      const refund = this.lookup(this.byResource.get(`refund:${liability.settlementSignature}`));
      if (refund && refund.status !== 'rejected') {
        throw new PolicyError(
          'refund_already_started',
          'Refund liability cannot be discharged after refund execution starts',
          409,
        );
      }
      liability.dischargedAt = new Date(now);
      liability.dischargeEvidenceHash = evidenceHash;
      liability.dischargeEvidence = structuredClone(evidence);
      liability.dischargeIdempotencyKey = idempotencyKey;
      liability.dischargeRequestHash = requestHash;
      this.liabilityDischargeByIdempotency.set(idempotencyKey, liability.id);
      return cloneLiability(liability);
    });
  }

  async reserveRefund(
    input: ReserveOperation,
    liabilityId: string,
    now: Date,
  ): Promise<OperationRecord> {
    return this.exclusive(() => {
      const idempotent = this.lookup(this.byIdempotency.get(input.idempotencyKey));
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        return clone(idempotent);
      }
      const liability = [...this.refundLiabilities.values()].find(
        (entry) => entry.id === liabilityId,
      );
      if (!liability || liability.settlementSignature !== input.details.settlementSignature) {
        throw new PolicyError('refund_liability_not_found', 'Refund liability was not found', 404);
      }
      if (liability.dischargedAt) {
        throw new PolicyError(
          'refund_liability_discharged',
          'Discharged refund liability cannot be executed',
          409,
        );
      }
      const resource = this.lookup(this.byResource.get(input.resourceKey));
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        this.archiveRejectedResource(resource);
      }
      const record = makeRecord(input, now);
      this.records.set(record.id, record);
      this.byIdempotency.set(record.idempotencyKey, record.id);
      this.byResource.set(record.resourceKey, record.id);
      return clone(record);
    });
  }

  async reserve(
    input: ReserveOperation,
    dailyLimitUsdCents: number,
    now: Date,
  ): Promise<OperationRecord> {
    return this.exclusive(() => {
      const idempotent = this.lookup(this.byIdempotency.get(input.idempotencyKey));
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        return clone(idempotent);
      }

      const resource = this.lookup(this.byResource.get(input.resourceKey));
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        this.archiveRejectedResource(resource);
      }

      const cutoff = now.getTime() - 24 * 60 * 60 * 1000;
      const reserved = [...this.records.values()]
        .filter(
          (record) =>
            record.spendBucket === input.spendBucket &&
            record.createdAt.getTime() >= cutoff &&
            record.status !== 'rejected',
        )
        .reduce((sum, record) => sum + record.amountUsdCents, 0);
      if (input.spendBucket !== 'none' && reserved + input.amountUsdCents > dailyLimitUsdCents) {
        throw new PolicyError(
          'daily_limit_exceeded',
          'Rolling 24-hour spending limit exceeded',
          429,
          true,
        );
      }

      const record: OperationRecord = {
        ...input,
        status: 'reserved',
        prepared: null,
        transactionSignature: null,
        errorCode: null,
        errorMessage: null,
        leaseOwner: null,
        leaseExpiresAt: null,
        createdAt: new Date(now),
        updatedAt: new Date(now),
        version: 0,
      };
      this.records.set(record.id, record);
      this.byIdempotency.set(record.idempotencyKey, record.id);
      this.byResource.set(record.resourceKey, record.id);
      return clone(record);
    });
  }

  async issueGitHubIdentityGrant(grant: GitHubIdentityGrant): Promise<GitHubIdentityGrant> {
    return this.exclusive(() => {
      if (this.githubIdentityGrants.has(grant.id)) {
        throw new PolicyError('grant_conflict', 'GitHub identity grant already exists', 409);
      }
      this.githubIdentityGrants.set(grant.id, cloneGrant(grant));
      return cloneGrant(grant);
    });
  }

  async getGitHubIdentityGrant(id: string): Promise<GitHubIdentityGrant | null> {
    return this.exclusive(() => {
      const grant = this.githubIdentityGrants.get(id);
      return grant ? cloneGrant(grant) : null;
    });
  }

  async issueBindChallenge(
    challenge: BindChallenge,
    grantId: string,
    now: Date,
  ): Promise<BindChallenge> {
    return this.exclusive(() => {
      if (this.bindChallenges.has(challenge.id)) {
        throw new PolicyError('challenge_conflict', 'Binding challenge already exists', 409);
      }
      const grant = this.githubIdentityGrants.get(grantId);
      if (
        !grant ||
        grant.githubId !== challenge.claimantGitHubId ||
        grant.login !== challenge.claimantGitHubLogin
      ) {
        throw new PolicyError('github_grant_invalid', 'GitHub identity grant is invalid', 422);
      }
      if (grant.consumedAt) {
        throw new PolicyError(
          'github_grant_consumed',
          'GitHub identity grant was already consumed',
          409,
        );
      }
      if (now.getTime() >= grant.expiresAt.getTime()) {
        throw new PolicyError('github_grant_expired', 'GitHub identity grant has expired', 422);
      }
      this.bindChallenges.set(challenge.id, cloneChallenge(challenge));
      grant.consumedAt = new Date(now);
      grant.challengeId = challenge.id;
      return cloneChallenge(challenge);
    });
  }

  async getBindChallenge(id: string): Promise<BindChallenge | null> {
    return this.exclusive(() => {
      const challenge = this.bindChallenges.get(id);
      return challenge ? cloneChallenge(challenge) : null;
    });
  }

  async reserveWithBindChallenge(
    input: ReserveOperation,
    challengeId: string,
    bindingHash: string,
    now: Date,
  ): Promise<OperationRecord> {
    return this.exclusive(() => {
      const idempotent = this.lookup(this.byIdempotency.get(input.idempotencyKey));
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        return clone(idempotent);
      }

      const challenge = this.bindChallenges.get(challengeId);
      if (!challenge || challenge.bindingHash !== bindingHash) {
        throw new PolicyError('challenge_invalid', 'Binding challenge is invalid', 422);
      }
      if (challenge.consumedAt) {
        throw new PolicyError('challenge_consumed', 'Binding challenge was already consumed', 409);
      }
      if (now.getTime() >= challenge.expiresAt.getTime()) {
        throw new PolicyError('challenge_expired', 'Binding challenge has expired', 422);
      }
      const resource = this.lookup(this.byResource.get(input.resourceKey));
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        this.archiveRejectedResource(resource);
      }

      const record = makeRecord(input, now);
      this.records.set(record.id, record);
      this.byIdempotency.set(record.idempotencyKey, record.id);
      this.byResource.set(record.resourceKey, record.id);
      challenge.consumedAt = new Date(now);
      challenge.bindOperationId = record.id;
      return clone(record);
    });
  }

  async get(id: string): Promise<OperationRecord | null> {
    return this.exclusive(() => {
      const record = this.records.get(id);
      return record ? clone(record) : null;
    });
  }

  async getByIdempotencyKey(key: string): Promise<OperationRecord | null> {
    return this.exclusive(() => {
      const record = this.lookup(this.byIdempotency.get(key));
      return record ? clone(record) : null;
    });
  }

  async getByResourceKey(key: string): Promise<OperationRecord | null> {
    return this.exclusive(() => {
      const record = this.lookup(this.byResource.get(key));
      return record ? clone(record) : null;
    });
  }

  async acquireLease(
    id: string,
    owner: string,
    now: Date,
    leaseMs: number,
  ): Promise<OperationRecord | null> {
    return this.exclusive(() => {
      const record = this.records.get(id);
      if (!record) return null;
      if (
        record.leaseOwner &&
        record.leaseOwner !== owner &&
        record.leaseExpiresAt &&
        record.leaseExpiresAt.getTime() > now.getTime()
      ) {
        return null;
      }
      record.leaseOwner = owner;
      record.leaseExpiresAt = new Date(now.getTime() + leaseMs);
      record.updatedAt = new Date(now);
      record.version += 1;
      return clone(record);
    });
  }

  async update(
    id: string,
    owner: string,
    expectedVersion: number,
    patch: OperationPatch,
  ): Promise<OperationRecord> {
    return this.exclusive(() => {
      const record = this.records.get(id);
      if (!record) throw new PolicyError('operation_not_found', 'Operation was not found', 404);
      if (record.leaseOwner !== owner) {
        throw new PolicyError(
          'lease_lost',
          'Operation lease is not held by this worker',
          409,
          true,
        );
      }
      if (record.version !== expectedVersion) {
        throw new PolicyError('version_conflict', 'Operation changed concurrently', 409, true);
      }

      applyPatch(record, patch);
      record.updatedAt = new Date();
      record.version += 1;
      return clone(record);
    });
  }

  async releaseLease(id: string, owner: string): Promise<void> {
    await this.exclusive(() => {
      const record = this.records.get(id);
      if (!record || record.leaseOwner !== owner) return;
      record.leaseOwner = null;
      record.leaseExpiresAt = null;
      record.updatedAt = new Date();
      record.version += 1;
    });
  }

  async listRecoverable(limit: number): Promise<OperationRecord[]> {
    return this.exclusive(() =>
      [...this.records.values()]
        .filter((record) => !['finalized', 'rejected'].includes(record.status))
        .sort(
          (left, right) =>
            left.updatedAt.getTime() - right.updatedAt.getTime() || left.id.localeCompare(right.id),
        )
        .slice(0, limit)
        .map(clone),
    );
  }

  async stats(): Promise<StoreStats> {
    return this.exclusive(() => {
      const byStatus: StoreStats['byStatus'] = {};
      for (const record of this.records.values()) {
        byStatus[record.status] = (byStatus[record.status] ?? 0) + 1;
      }
      return { total: this.records.size, byStatus };
    });
  }

  async pendingRefundRawAmount(): Promise<string> {
    return this.exclusive(() => {
      let total = 0n;
      for (const liability of this.refundLiabilities.values()) {
        if (this.isLiabilityOutstanding(liability)) total += BigInt(liability.rawAmount);
      }
      return total.toString();
    });
  }

  async rollingSpendUsdCents(bucket: 'refund' | 'escrow', now: Date): Promise<number> {
    return this.exclusive(() => {
      const cutoff = now.getTime() - 24 * 60 * 60 * 1_000;
      if (bucket === 'refund') {
        return [...this.refundLiabilities.values()]
          .filter((liability) => liability.createdAt.getTime() >= cutoff)
          .reduce((total, liability) => total + liability.amountUsdCents, 0);
      }
      return [...this.records.values()]
        .filter(
          (record) =>
            record.spendBucket === bucket &&
            record.status !== 'rejected' &&
            record.createdAt.getTime() >= cutoff,
        )
        .reduce((total, record) => total + record.amountUsdCents, 0);
    });
  }

  async ping(): Promise<void> {}
  async close(): Promise<void> {}

  private lookup(id: string | undefined): OperationRecord | null {
    return id ? (this.records.get(id) ?? null) : null;
  }

  private archiveRejectedResource(record: OperationRecord): void {
    this.byResource.delete(record.resourceKey);
    record.resourceKey = `rejected:${record.id}:${record.resourceKey}`;
    this.byResource.set(record.resourceKey, record.id);
  }

  private isLiabilityOutstanding(liability: RefundLiability): boolean {
    if (liability.dischargedAt) return false;
    const operation = this.lookup(this.byResource.get(`refund:${liability.settlementSignature}`));
    return operation?.status !== 'finalized';
  }

  private async exclusive<T>(fn: () => T | Promise<T>): Promise<T> {
    const previous = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await fn();
    } finally {
      release();
    }
  }
}

function applyPatch(record: OperationRecord, patch: OperationPatch): void {
  if (patch.status !== undefined) record.status = patch.status;
  if (patch.prepared !== undefined) record.prepared = patch.prepared;
  if (patch.transactionSignature !== undefined) {
    record.transactionSignature = patch.transactionSignature;
  }
  if (patch.errorCode !== undefined) record.errorCode = patch.errorCode;
  if (patch.errorMessage !== undefined) record.errorMessage = patch.errorMessage;
  if (patch.details !== undefined) record.details = patch.details;
}

function clone(record: OperationRecord): OperationRecord {
  return {
    ...record,
    details: structuredClone(record.details),
    prepared: record.prepared ? { ...record.prepared } : null,
    leaseExpiresAt: record.leaseExpiresAt ? new Date(record.leaseExpiresAt) : null,
    createdAt: new Date(record.createdAt),
    updatedAt: new Date(record.updatedAt),
  };
}

function makeRecord(input: ReserveOperation, now: Date): OperationRecord {
  return {
    ...input,
    status: 'reserved',
    prepared: null,
    transactionSignature: null,
    errorCode: null,
    errorMessage: null,
    leaseOwner: null,
    leaseExpiresAt: null,
    createdAt: new Date(now),
    updatedAt: new Date(now),
    version: 0,
  };
}

function cloneChallenge(challenge: BindChallenge): BindChallenge {
  return {
    ...challenge,
    claimExpiresAt: new Date(challenge.claimExpiresAt),
    issuedAt: new Date(challenge.issuedAt),
    expiresAt: new Date(challenge.expiresAt),
    consumedAt: challenge.consumedAt ? new Date(challenge.consumedAt) : null,
  };
}

function cloneGrant(grant: GitHubIdentityGrant): GitHubIdentityGrant {
  return {
    ...grant,
    issuedAt: new Date(grant.issuedAt),
    expiresAt: new Date(grant.expiresAt),
    consumedAt: grant.consumedAt ? new Date(grant.consumedAt) : null,
  };
}

function cloneLiability(liability: RefundLiability): RefundLiability {
  return {
    ...liability,
    repositoryAuthorizedAt: new Date(liability.repositoryAuthorizedAt),
    deliveryBoundAt: liability.deliveryBoundAt ? new Date(liability.deliveryBoundAt) : null,
    createdAt: new Date(liability.createdAt),
    dischargedAt: liability.dischargedAt ? new Date(liability.dischargedAt) : null,
    dischargeEvidence: liability.dischargeEvidence
      ? structuredClone(liability.dischargeEvidence)
      : null,
  };
}

function cloneAdmission(admission: RepositoryAdmission): RepositoryAdmission {
  return {
    ...admission,
    permissions: { ...admission.permissions },
    tokenExpiresAt: new Date(admission.tokenExpiresAt),
    admittedAt: new Date(admission.admittedAt),
  };
}
