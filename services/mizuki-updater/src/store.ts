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
}

export interface PromotionControlUpdate {
  promotionsEnabled: boolean;
  expectedRevision: number;
  reason: string;
  updatedBy: string;
}

export type PromotionAdmission<T> =
  | { admitted: true; value: T }
  | { admitted: false; control: PromotionControl };

export interface UpgradeRepository {
  migrate(): Promise<void>;
  promotionControl(): Promise<PromotionControl>;
  updatePromotionControl(input: PromotionControlUpdate, now: Date): Promise<PromotionControl>;
  withPromotionAdmission<T>(action: () => Promise<T>): Promise<PromotionAdmission<T>>;
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
  };
  private promotionGate = Promise.resolve();

  async migrate(): Promise<void> {}

  async promotionControl(): Promise<PromotionControl> {
    return structuredClone(this.control);
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
      this.control = {
        promotionsEnabled: input.promotionsEnabled,
        revision: this.control.revision + 1,
        reason: input.reason,
        updatedBy: input.updatedBy,
        updatedAt: now,
      };
      return structuredClone(this.control);
    });
  }

  async withPromotionAdmission<T>(action: () => Promise<T>): Promise<PromotionAdmission<T>> {
    return this.withPromotionGate(async () => {
      if (!this.control.promotionsEnabled) {
        return { admitted: false, control: structuredClone(this.control) };
      }
      return { admitted: true, value: await action() };
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
