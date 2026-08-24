import {
  assertStateTransition,
  createAuditReceipt,
  type AuditEvent,
  type AuditReceipt,
  type NewUpgrade,
  type UpgradePatch,
  type UpgradeRecord,
  type UpgradeState,
  type UpgradeStats,
  UpdaterError,
} from './domain.js';

export interface PromotionControl {
  promotionsEnabled: boolean;
  revision: number;
  reason: string;
  updatedBy: string;
  updatedAt: Date;
  activeUpgradeId: string | null;
  activeSince: Date | null;
}

export interface PromotionControlUpdate {
  promotionsEnabled: boolean;
  expectedRevision: number;
  reason: string;
  updatedBy: string;
}

export interface PromotionFailureResolution {
  upgradeId: string;
  expectedRevision: number;
  reason: string;
  updatedBy: string;
}

export interface PromotionControlAuditEntry extends PromotionControl {
  sequence: number;
}

export type PromotionReservation =
  | { reserved: true; control: PromotionControl }
  | { reserved: false; reason: 'disabled' | 'busy'; control: PromotionControl };

export interface UpgradeRepository {
  migrate(): Promise<void>;
  promotionControl(): Promise<PromotionControl>;
  promotionControlAudit(limit?: number): Promise<PromotionControlAuditEntry[]>;
  updatePromotionControl(input: PromotionControlUpdate, now: Date): Promise<PromotionControl>;
  reservePromotion(upgradeId: string, now: Date): Promise<PromotionReservation>;
  releasePromotion(upgradeId: string, now: Date): Promise<void>;
  closePromotionsForFailure(
    upgradeId: string,
    reason: string,
    now: Date,
  ): Promise<PromotionControl>;
  resolvePromotionFailure(input: PromotionFailureResolution, now: Date): Promise<PromotionControl>;
  reserve(input: NewUpgrade, now: Date): Promise<UpgradeRecord>;
  get(id: string): Promise<UpgradeRecord | null>;
  getByProposalId(proposalId: string): Promise<UpgradeRecord | null>;
  audit(id: string): Promise<AuditReceipt[]>;
  acquireLease(id: string, owner: string, now: Date, leaseMs: number): Promise<boolean>;
  releaseLease(id: string, owner: string): Promise<void>;
  transition(
    id: string,
    expectedVersion: number,
    leaseOwner: string,
    patch: UpgradePatch,
    event: AuditEvent,
    now: Date,
  ): Promise<UpgradeRecord>;
  listRunnable(now: Date, limit: number): Promise<string[]>;
  stats(): Promise<UpgradeStats>;
  health(): Promise<void>;
  close(): Promise<void>;
}

const terminalStates = new Set<UpgradeState>([
  'completed',
  'rolled_back',
  'failed',
  'rollback_failed',
]);

export class InMemoryUpgradeRepository implements UpgradeRepository {
  private readonly records = new Map<string, UpgradeRecord>();
  private readonly idsByIdempotency = new Map<string, string>();
  private readonly idsByProposal = new Map<string, string>();
  private readonly receipts = new Map<string, AuditReceipt[]>();
  private control: PromotionControl = {
    promotionsEnabled: false,
    revision: 0,
    reason: 'promotions are closed until explicitly enabled',
    updatedBy: 'system',
    updatedAt: new Date(0),
    activeUpgradeId: null,
    activeSince: null,
  };
  private readonly controlAudit: PromotionControlAuditEntry[] = [
    { ...structuredClone(this.control), sequence: 1 },
  ];
  private promotionGate = Promise.resolve();

  async migrate(): Promise<void> {}

  async promotionControl(): Promise<PromotionControl> {
    return structuredClone(this.control);
  }

  async promotionControlAudit(limit = 100): Promise<PromotionControlAuditEntry[]> {
    return structuredClone(this.controlAudit.slice(-limit));
  }

  async updatePromotionControl(
    input: PromotionControlUpdate,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionGate(async () => {
      if (this.control.revision !== input.expectedRevision) {
        throw new UpdaterError(
          'promotion_control_conflict',
          'Promotion control changed concurrently',
          409,
        );
      }
      if (
        input.promotionsEnabled &&
        this.control.activeUpgradeId &&
        this.records.get(this.control.activeUpgradeId)?.state === 'rollback_failed'
      ) {
        throw new UpdaterError(
          'promotion_failure_unresolved',
          'The failed rollback must be explicitly resolved before promotions can be enabled',
          409,
        );
      }
      this.control = {
        ...this.control,
        promotionsEnabled: input.promotionsEnabled,
        revision: this.control.revision + 1,
        reason: input.reason,
        updatedBy: input.updatedBy,
        updatedAt: now,
      };
      this.appendControlAudit();
      return structuredClone(this.control);
    });
  }

  async reservePromotion(upgradeId: string, now: Date): Promise<PromotionReservation> {
    return this.withPromotionGate(async () => {
      if (!this.records.has(upgradeId)) {
        throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
      }
      await this.reconcileReservation(now);
      if (!this.control.promotionsEnabled) {
        return { reserved: false, reason: 'disabled', control: structuredClone(this.control) };
      }
      if (this.control.activeUpgradeId && this.control.activeUpgradeId !== upgradeId) {
        return { reserved: false, reason: 'busy', control: structuredClone(this.control) };
      }
      if (!this.control.activeUpgradeId) {
        this.control = {
          ...this.control,
          activeUpgradeId: upgradeId,
          activeSince: now,
          revision: this.control.revision + 1,
          reason: 'promotion reservation acquired',
          updatedBy: `updater:${upgradeId}`,
          updatedAt: now,
        };
        this.appendControlAudit();
      }
      return { reserved: true, control: structuredClone(this.control) };
    });
  }

  async releasePromotion(upgradeId: string, now: Date): Promise<void> {
    await this.withPromotionGate(async () => {
      if (this.control.activeUpgradeId !== upgradeId) return;
      const state = this.records.get(upgradeId)?.state;
      if (!state || !['completed', 'rolled_back', 'failed'].includes(state)) {
        throw new UpdaterError(
          'promotion_release_not_terminal',
          'Promotion reservation cannot be released before a terminal outcome',
          409,
        );
      }
      this.control = {
        ...this.control,
        activeUpgradeId: null,
        activeSince: null,
        revision: this.control.revision + 1,
        reason: 'promotion reservation released',
        updatedBy: `updater:${upgradeId}`,
        updatedAt: now,
      };
      this.appendControlAudit();
    });
  }

  async closePromotionsForFailure(
    upgradeId: string,
    reason: string,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionGate(async () => {
      if (this.control.activeUpgradeId && this.control.activeUpgradeId !== upgradeId) {
        throw new UpdaterError(
          'promotion_reservation_mismatch',
          'Another upgrade owns the promotion reservation',
          409,
        );
      }
      this.control = {
        ...this.control,
        promotionsEnabled: false,
        revision: this.control.revision + 1,
        reason,
        updatedBy: `updater:${upgradeId}`,
        updatedAt: now,
        activeUpgradeId: upgradeId,
        activeSince: this.control.activeSince ?? now,
      };
      this.appendControlAudit();
      return structuredClone(this.control);
    });
  }

  async resolvePromotionFailure(
    input: PromotionFailureResolution,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionGate(async () => {
      if (this.control.revision !== input.expectedRevision) {
        throw new UpdaterError(
          'promotion_control_conflict',
          'Promotion control changed concurrently',
          409,
        );
      }
      if (this.control.promotionsEnabled) {
        throw new UpdaterError(
          'promotion_failure_resolution_invalid',
          'Promotions must remain disabled while resolving a failed rollback',
          409,
        );
      }
      if (
        this.control.activeUpgradeId !== input.upgradeId ||
        this.records.get(input.upgradeId)?.state !== 'rollback_failed'
      ) {
        throw new UpdaterError(
          'promotion_failure_resolution_invalid',
          'The upgrade does not own an unresolved failed rollback',
          409,
        );
      }
      this.control = {
        ...this.control,
        activeUpgradeId: null,
        activeSince: null,
        revision: this.control.revision + 1,
        reason: input.reason,
        updatedBy: input.updatedBy,
        updatedAt: now,
      };
      this.appendControlAudit();
      return structuredClone(this.control);
    });
  }

  async reserve(input: NewUpgrade, now: Date): Promise<UpgradeRecord> {
    const byKey = this.idsByIdempotency.get(input.idempotencyKey);
    if (byKey) return this.assertIdempotent(byKey, input.requestHash);
    const byProposal = this.idsByProposal.get(input.proposalId);
    if (byProposal) return this.assertIdempotent(byProposal, input.requestHash);

    const record: UpgradeRecord = {
      ...input,
      state: 'submitted',
      prNumber: null,
      prUrl: null,
      deploymentId: null,
      mergeSha: null,
      promotionOperationId: null,
      promotionHealthyAt: null,
      waitStartedAt: null,
      nextAttemptAt: null,
      attemptCount: 0,
      lastErrorCode: null,
      lastErrorMessage: null,
      leaseOwner: null,
      leaseExpiresAt: null,
      version: 0,
      createdAt: now,
      updatedAt: now,
    };
    const receipt = createAuditReceipt(
      record.id,
      1,
      null,
      'submitted',
      { event: 'proposal_submitted', details: { manifestSha256: input.envelope.manifestSha256 } },
      now,
      null,
    );
    this.records.set(record.id, record);
    this.idsByIdempotency.set(input.idempotencyKey, record.id);
    this.idsByProposal.set(input.proposalId, record.id);
    this.receipts.set(record.id, [receipt]);
    return cloneRecord(record);
  }

  async get(id: string): Promise<UpgradeRecord | null> {
    const record = this.records.get(id);
    return record ? cloneRecord(record) : null;
  }

  async getByProposalId(proposalId: string): Promise<UpgradeRecord | null> {
    const id = this.idsByProposal.get(proposalId);
    return id ? this.get(id) : null;
  }

  async audit(id: string): Promise<AuditReceipt[]> {
    return (this.receipts.get(id) ?? []).map(cloneReceipt);
  }

  async acquireLease(id: string, owner: string, now: Date, leaseMs: number): Promise<boolean> {
    const record = this.records.get(id);
    if (!record || terminalStates.has(record.state)) return false;
    if (
      record.leaseOwner !== null &&
      record.leaseOwner !== owner &&
      record.leaseExpiresAt !== null &&
      record.leaseExpiresAt > now
    ) {
      return false;
    }
    record.leaseOwner = owner;
    record.leaseExpiresAt = new Date(now.getTime() + leaseMs);
    return true;
  }

  async releaseLease(id: string, owner: string): Promise<void> {
    const record = this.records.get(id);
    if (record?.leaseOwner === owner) {
      record.leaseOwner = null;
      record.leaseExpiresAt = null;
    }
  }

  async transition(
    id: string,
    expectedVersion: number,
    leaseOwner: string,
    patch: UpgradePatch,
    event: AuditEvent,
    now: Date,
  ): Promise<UpgradeRecord> {
    const record = this.records.get(id);
    if (!record) throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
    if (record.leaseOwner !== leaseOwner) {
      throw new UpdaterError('lease_lost', 'Upgrade lease is not held', 409, true);
    }
    if (record.version !== expectedVersion) {
      throw new UpdaterError('version_conflict', 'Upgrade changed concurrently', 409, true);
    }

    const fromState = record.state;
    assertStateTransition(fromState, patch.state ?? fromState);
    Object.assign(record, patch, { version: record.version + 1, updatedAt: now });
    const list = this.receipts.get(id)!;
    const previous = list.at(-1) ?? null;
    list.push(
      createAuditReceipt(
        id,
        list.length + 1,
        fromState,
        record.state,
        event,
        now,
        previous?.hash ?? null,
      ),
    );
    return cloneRecord(record);
  }

  async listRunnable(now: Date, limit: number): Promise<string[]> {
    return [...this.records.values()]
      .filter(
        (record) =>
          !terminalStates.has(record.state) &&
          (record.nextAttemptAt === null || record.nextAttemptAt <= now) &&
          (record.leaseExpiresAt === null || record.leaseExpiresAt <= now),
      )
      .sort((left, right) => left.updatedAt.getTime() - right.updatedAt.getTime())
      .slice(0, limit)
      .map((record) => record.id);
  }

  async stats(): Promise<UpgradeStats> {
    const byState: Partial<Record<UpgradeState, number>> = {};
    for (const record of this.records.values()) {
      byState[record.state] = (byState[record.state] ?? 0) + 1;
    }
    return { total: this.records.size, byState };
  }

  async health(): Promise<void> {}

  async close(): Promise<void> {}

  private assertIdempotent(id: string, requestHash: string): UpgradeRecord {
    const existing = this.records.get(id)!;
    if (existing.requestHash !== requestHash) {
      throw new UpdaterError(
        'idempotency_conflict',
        'Proposal or idempotency key was already used for different content',
        409,
      );
    }
    return cloneRecord(existing);
  }

  private async withPromotionGate<T>(action: () => Promise<T>): Promise<T> {
    let release = () => {};
    const previous = this.promotionGate;
    this.promotionGate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await action();
    } finally {
      release();
    }
  }

  private async reconcileReservation(now: Date): Promise<void> {
    const id = this.control.activeUpgradeId;
    if (!id) return;
    const state = this.records.get(id)?.state;
    if (state === 'rollback_failed') {
      if (this.control.promotionsEnabled) {
        this.control = {
          ...this.control,
          promotionsEnabled: false,
          revision: this.control.revision + 1,
          reason: 'promotion rollback requires operator intervention',
          updatedBy: `updater:${id}`,
          updatedAt: now,
        };
        this.appendControlAudit();
      }
      return;
    }
    if (state && terminalStates.has(state)) {
      this.control = {
        ...this.control,
        activeUpgradeId: null,
        activeSince: null,
        revision: this.control.revision + 1,
        reason: `terminal promotion reservation reconciled: ${state}`,
        updatedBy: `updater:${id}`,
        updatedAt: now,
      };
      this.appendControlAudit();
    }
  }

  private appendControlAudit(): void {
    this.controlAudit.push({
      ...structuredClone(this.control),
      sequence: this.controlAudit.length + 1,
    });
  }
}

function cloneRecord(record: UpgradeRecord): UpgradeRecord {
  return structuredClone(record);
}

function cloneReceipt(receipt: AuditReceipt): AuditReceipt {
  return structuredClone(receipt);
}

export function isTerminalState(state: UpgradeState): boolean {
  return terminalStates.has(state);
}
