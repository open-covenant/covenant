import { createHash, randomUUID } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { Pool, type PoolClient } from 'pg';
import type {
  Capability,
  ContributorEscrow,
  FailureRecord,
  RescueBounty,
  Upgrade,
} from './domain/index.js';
import type {
  ActivityEvent,
  ActivityKind,
  AccountRepository,
  Contributor,
  GithubOAuthFlow,
  Job,
  JobState,
  LedgerEntry,
  OperatorControlAuditEntry,
  OperatorControls,
  OperatorControlsPatch,
  Payment,
  Quote,
  RepositoryAdmissionReceipt,
  WalletChallenge,
} from './types.js';

export const MAX_PENDING_GITHUB_OAUTH_FLOWS = 1_000;

export type JobPatch = Omit<Partial<Job>, 'id' | 'state' | 'version' | 'createdAt' | 'updatedAt'>;

export type AccountJobsPage = {
  jobs: Job[];
  limit: number;
  truncated: boolean;
  obligationCount: number;
};

export type AccountRepositoriesPage = {
  repositories: AccountRepository[];
  limit: number;
  truncated: boolean;
};

export type AccountBountiesPage = {
  bounties: RescueBounty[];
  limit: number;
  truncated: boolean;
};

export type WebhookDeliveryLease =
  | { state: 'started'; leaseId: string }
  | { state: 'completed' | 'busy' };

export interface MizukiStore {
  readiness(): Promise<void>;
  withAdmissionLock<T>(operation: () => Promise<T>): Promise<T>;
  operatorControls(): Promise<OperatorControls>;
  operatorControlsAudit(): Promise<OperatorControlAuditEntry[]>;
  updateOperatorControls(patch: OperatorControlsPatch): Promise<OperatorControls>;
  saveQuote(quote: Quote): Promise<Quote>;
  quote(id: string): Promise<Quote | undefined>;
  linkQuoteToAccount(quoteId: string, githubId: string): Promise<void>;
  quoteForAccount(quoteId: string, githubId: string): Promise<Quote | undefined>;
  jobsForAccount(githubId: string, limit: number): Promise<AccountJobsPage>;
  linkAccountRepository(githubId: string, owner: string, repo: string): Promise<AccountRepository>;
  repositoriesForAccount(githubId: string, limit: number): Promise<AccountRepositoriesPage>;
  createJob(
    quote: Quote,
    payment: Payment,
    idempotencyKey: string,
    repositoryAdmission?: RepositoryAdmissionReceipt,
  ): Promise<{ job: Job; created: boolean }>;
  job(id: string): Promise<Job | undefined>;
  jobByIdempotencyKey(key: string): Promise<Job | undefined>;
  jobByQuote(quoteId: string): Promise<Job | undefined>;
  jobsList(): Promise<Job[]>;
  transitionJob(
    id: string,
    expected: JobState | readonly JobState[],
    state: JobState,
    patch?: JobPatch,
  ): Promise<Job>;
  patchJob(id: string, patch: JobPatch): Promise<Job>;
  appendLedger(entry: Omit<LedgerEntry, 'id' | 'createdAt'>): Promise<LedgerEntry>;
  ledgerEntries(): Promise<LedgerEntry[]>;
  appendActivity(
    kind: ActivityKind,
    subjectId: string,
    publicData?: Record<string, unknown>,
    eventId?: string,
  ): Promise<ActivityEvent>;
  activity(limit?: number): Promise<ActivityEvent[]>;
  upsertContributor(githubId: string, githubLogin: string): Promise<Contributor>;
  contributor(githubId: string): Promise<Contributor | undefined>;
  saveGithubOAuthFlow(flow: GithubOAuthFlow): Promise<GithubOAuthFlow>;
  consumeGithubOAuthFlow(id: string, binding: string): Promise<GithubOAuthFlow>;
  saveWalletChallenge(challenge: WalletChallenge): Promise<WalletChallenge>;
  walletChallenge(id: string, githubId: string): Promise<WalletChallenge | undefined>;
  consumeWalletChallenge(id: string, githubId: string): Promise<WalletChallenge>;
  linkContributorWallet(githubId: string, wallet: string): Promise<Contributor>;
  beginWebhookDelivery(deliveryId: string): Promise<WebhookDeliveryLease>;
  completeWebhookDelivery(deliveryId: string, leaseId: string): Promise<void>;
  failWebhookDelivery(deliveryId: string, leaseId: string, error: string): Promise<void>;
  createBounty(bounty: RescueBounty): Promise<{ bounty: RescueBounty; created: boolean }>;
  bounty(id: string): Promise<RescueBounty | undefined>;
  bountyBySourceJob(jobId: string): Promise<RescueBounty | undefined>;
  bountiesList(): Promise<RescueBounty[]>;
  bountiesForAccount(githubId: string, limit: number): Promise<AccountBountiesPage>;
  updateBounty(bounty: RescueBounty, expectedRevision: number): Promise<RescueBounty>;
  saveEscrow(escrow: ContributorEscrow): Promise<ContributorEscrow>;
  escrow(id: string): Promise<ContributorEscrow | undefined>;
  escrowByBounty(bountyId: string): Promise<ContributorEscrow | undefined>;
  escrowsByBounty(bountyId: string): Promise<ContributorEscrow[]>;
  saveCapability(capability: Capability): Promise<Capability>;
  capabilitiesList(): Promise<Capability[]>;
  saveUpgrade(upgrade: Upgrade): Promise<Upgrade>;
  upgradesList(): Promise<Upgrade[]>;
  saveFailure(failure: FailureRecord): Promise<FailureRecord>;
  failuresForCapability(capabilityKey: string): Promise<FailureRecord[]>;
  capabilityByKey(key: string): Promise<Capability | undefined>;
  close(): Promise<void>;
}

export class MemoryStore implements MizukiStore {
  private readonly quotes = new Map<string, Quote>();
  private readonly jobs = new Map<string, Job>();
  private readonly ledger: LedgerEntry[] = [];
  private readonly events: ActivityEvent[] = [];
  private readonly contributors = new Map<string, Contributor>();
  private readonly quoteAccounts = new Map<string, string>();
  private readonly accountRepositories = new Map<string, AccountRepository>();
  private readonly githubOAuthFlows = new Map<string, GithubOAuthFlow>();
  private readonly challenges = new Map<string, WalletChallenge>();
  private readonly webhookDeliveries = new Map<
    string,
    { status: 'processing' | 'completed' | 'failed'; leaseId: string; updatedAt: number }
  >();
  private readonly bounties = new Map<string, RescueBounty>();
  private readonly escrows = new Map<string, ContributorEscrow>();
  private readonly capabilities = new Map<string, Capability>();
  private readonly upgrades = new Map<string, Upgrade>();
  private readonly failures: FailureRecord[] = [];
  private controls: OperatorControls = initialOperatorControls();
  private readonly controlsAudit: OperatorControlAuditEntry[] = [
    { ...structuredClone(this.controls), expectedRevision: 0 },
  ];

  async readiness(): Promise<void> {}

  async withAdmissionLock<T>(operation: () => Promise<T>): Promise<T> {
    return operation();
  }

  async operatorControls(): Promise<OperatorControls> {
    const latest = this.controlsAudit.at(-1);
    if (!latest) throw new Error('operator admission controls are unavailable or unaudited');
    const audited: OperatorControls = {
      intakeEnabled: latest.intakeEnabled,
      claimsEnabled: latest.claimsEnabled,
      revision: latest.revision,
      reason: latest.reason,
      updatedBy: latest.updatedBy,
      updatedAt: latest.updatedAt,
    };
    if (!isDeepStrictEqual(this.controls, audited)) {
      throw new Error('operator admission controls are unavailable or unaudited');
    }
    return structuredClone(this.controls);
  }

  async operatorControlsAudit(): Promise<OperatorControlAuditEntry[]> {
    return structuredClone(this.controlsAudit);
  }

  async updateOperatorControls(patch: OperatorControlsPatch): Promise<OperatorControls> {
    this.controls = updatedOperatorControls(this.controls, patch);
    this.controlsAudit.push({
      ...structuredClone(this.controls),
      expectedRevision: patch.expectedRevision,
    });
    return structuredClone(this.controls);
  }

  async saveQuote(quote: Quote): Promise<Quote> {
    this.quotes.set(quote.id, structuredClone(quote));
    return quote;
  }

  async quote(id: string): Promise<Quote | undefined> {
    return clone(this.quotes.get(id));
  }

  async linkQuoteToAccount(quoteId: string, githubId: string): Promise<void> {
    if (!this.quotes.has(quoteId)) throw new Error('quote not found');
    if (!this.contributors.has(githubId)) throw new Error('account not found');
    const current = this.quoteAccounts.get(quoteId);
    if (current && current !== githubId) {
      throw new StateConflictError('quote is already linked to another account');
    }
    this.quoteAccounts.set(quoteId, githubId);
  }

  async quoteForAccount(quoteId: string, githubId: string): Promise<Quote | undefined> {
    if (this.quoteAccounts.get(quoteId) !== githubId) return undefined;
    return clone(this.quotes.get(quoteId));
  }

  async jobsForAccount(githubId: string, limit: number): Promise<AccountJobsPage> {
    const boundedLimit = accountJobLimit(limit);
    const accountJobs = [...this.jobs.values()]
      .filter((job) => this.quoteAccounts.get(job.quote.id) === githubId)
      .sort(accountJobOrder);
    const obligations = accountJobs.filter(accountJobIsObligation);
    const terminal = accountJobs.filter((job) => !accountJobIsObligation(job));
    const history = terminal.slice(0, boundedLimit);
    return {
      jobs: [...obligations, ...history].sort(accountJobOrder).map((job) => structuredClone(job)),
      limit: boundedLimit,
      truncated: terminal.length > boundedLimit,
      obligationCount: obligations.length,
    };
  }

  async linkAccountRepository(
    githubId: string,
    owner: string,
    repo: string,
  ): Promise<AccountRepository> {
    if (!this.contributors.has(githubId)) throw new Error('account not found');
    const repository = normalizedRepository(owner, repo);
    const key = `${githubId}:${repository}`;
    if (!this.accountRepositories.has(key)) {
      const count = [...this.accountRepositories.values()].filter(
        (candidate) => candidate.githubId === githubId,
      ).length;
      if (count >= 25) throw new StateConflictError('account repository limit of 25 reached');
    }
    const value = {
      githubId,
      owner,
      repo,
      repository,
      verifiedAt: new Date().toISOString(),
    };
    this.accountRepositories.set(key, value);
    return structuredClone(value);
  }

  async repositoriesForAccount(githubId: string, limit: number): Promise<AccountRepositoriesPage> {
    const boundedLimit = accountRepositoryLimit(limit);
    const repositories = [...this.accountRepositories.values()]
      .filter((repository) => repository.githubId === githubId)
      .sort((left, right) => left.repository.localeCompare(right.repository))
      .slice(0, boundedLimit + 1)
      .map((repository) => structuredClone(repository));
    return {
      repositories: repositories.slice(0, boundedLimit),
      limit: boundedLimit,
      truncated: repositories.length > boundedLimit,
    };
  }

  async createJob(
    quote: Quote,
    payment: Payment,
    idempotencyKey: string,
    repositoryAdmission?: RepositoryAdmissionReceipt,
  ): Promise<{ job: Job; created: boolean }> {
    const proofHash = paymentProofHash(payment);
    const candidates = [...this.jobs.values()].filter(
      (job) =>
        job.quote.id === quote.id ||
        job.idempotencyKey === idempotencyKey ||
        (proofHash !== undefined && paymentProofHash(job.payment) === proofHash),
    );
    const conflicting = candidates.find(
      (job) =>
        (job.idempotencyKey === idempotencyKey ||
          (proofHash !== undefined && paymentProofHash(job.payment) === proofHash)) &&
        job.quote.id !== quote.id,
    );
    if (conflicting) throw new StateConflictError('payment reservation belongs to another quote');
    const existing = candidates.find((job) => job.quote.id === quote.id);
    if (existing) {
      if (
        repositoryAdmission &&
        existing.repositoryAdmission?.evidenceHash !== repositoryAdmission.evidenceHash
      ) {
        throw new StateConflictError('job reservation has different repository admission');
      }
      return { job: structuredClone(existing), created: false };
    }
    const now = new Date().toISOString();
    const job: Job = {
      id: randomUUID(),
      idempotencyKey,
      quote: structuredClone(quote),
      payment: structuredClone(payment),
      ...(repositoryAdmission ? { repositoryAdmission: structuredClone(repositoryAdmission) } : {}),
      state: 'settlement_pending',
      createdAt: now,
      updatedAt: now,
      inputTokens: 0,
      outputTokens: 0,
      estimatedCostUsd: 0,
      version: 0,
    };
    this.jobs.set(job.id, job);
    return { job: structuredClone(job), created: true };
  }

  async job(id: string): Promise<Job | undefined> {
    return clone(this.jobs.get(id));
  }

  async jobByIdempotencyKey(key: string): Promise<Job | undefined> {
    return clone([...this.jobs.values()].find((job) => job.idempotencyKey === key));
  }

  async jobByQuote(quoteId: string): Promise<Job | undefined> {
    return clone([...this.jobs.values()].find((job) => job.quote.id === quoteId));
  }

  async jobsList(): Promise<Job[]> {
    return [...this.jobs.values()]
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
      .map((job) => structuredClone(job));
  }

  async transitionJob(
    id: string,
    expected: JobState | readonly JobState[],
    state: JobState,
    patch: JobPatch = {},
  ): Promise<Job> {
    const current = this.jobs.get(id);
    if (!current) throw new Error(`unknown job: ${id}`);
    const allowed = Array.isArray(expected) ? expected : [expected];
    if (!allowed.includes(current.state)) {
      throw new StateConflictError(`job ${id} is ${current.state}; expected ${allowed.join(', ')}`);
    }
    const job = updateJob(current, state, patch);
    this.jobs.set(id, job);
    return structuredClone(job);
  }

  async patchJob(id: string, patch: JobPatch): Promise<Job> {
    const current = this.jobs.get(id);
    if (!current) throw new Error(`unknown job: ${id}`);
    const job = updateJob(current, current.state, patch);
    this.jobs.set(id, job);
    return structuredClone(job);
  }

  async appendLedger(entry: Omit<LedgerEntry, 'id' | 'createdAt'>): Promise<LedgerEntry> {
    const existing = this.ledger.find(
      (candidate) => candidate.kind === entry.kind && candidate.referenceId === entry.referenceId,
    );
    if (existing) {
      if (
        existing.asset !== entry.asset ||
        existing.amountAtomic !== entry.amountAtomic ||
        existing.amountUsd !== entry.amountUsd ||
        existing.transaction !== entry.transaction
      ) {
        throw new StateConflictError('ledger idempotency key reused with different values');
      }
      return structuredClone(existing);
    }
    const stored = { ...entry, id: randomUUID(), createdAt: new Date().toISOString() };
    this.ledger.push(stored);
    return structuredClone(stored);
  }

  async ledgerEntries(): Promise<LedgerEntry[]> {
    return this.ledger.map((entry) => structuredClone(entry));
  }

  async appendActivity(
    kind: ActivityKind,
    subjectId: string,
    publicData: Record<string, unknown> = {},
    eventId = randomUUID(),
  ): Promise<ActivityEvent> {
    const existing = this.events.find((event) => event.id === eventId);
    if (existing) {
      if (
        existing.kind !== kind ||
        existing.subjectId !== subjectId ||
        !isDeepStrictEqual(existing.publicData, publicData)
      ) {
        throw new StateConflictError('activity event id reused with different values');
      }
      return structuredClone(existing);
    }
    const event = {
      id: eventId,
      kind,
      subjectId,
      publicData,
      createdAt: new Date().toISOString(),
    };
    this.events.push(event);
    return structuredClone(event);
  }

  async activity(limit = 100): Promise<ActivityEvent[]> {
    return this.events
      .slice(-Math.max(0, limit))
      .reverse()
      .map((event) => structuredClone(event));
  }

  async upsertContributor(githubId: string, githubLogin: string): Promise<Contributor> {
    const current = this.contributors.get(githubId);
    const now = new Date().toISOString();
    const contributor = current
      ? { ...current, githubLogin, updatedAt: now }
      : { githubId, githubLogin, createdAt: now, updatedAt: now };
    this.contributors.set(githubId, contributor);
    return structuredClone(contributor);
  }

  async contributor(githubId: string): Promise<Contributor | undefined> {
    return clone(this.contributors.get(githubId));
  }

  async saveGithubOAuthFlow(flow: GithubOAuthFlow): Promise<GithubOAuthFlow> {
    for (const [id, current] of this.githubOAuthFlows) {
      if (current.consumedAt || Date.parse(current.expiresAt) <= Date.now()) {
        this.githubOAuthFlows.delete(id);
      }
    }
    if (this.githubOAuthFlows.has(flow.id)) {
      throw new StateConflictError('OAuth browser flow already exists');
    }
    if (this.githubOAuthFlows.size >= MAX_PENDING_GITHUB_OAUTH_FLOWS) {
      throw new GithubOAuthCapacityError();
    }
    this.githubOAuthFlows.set(flow.id, structuredClone(flow));
    return structuredClone(flow);
  }

  async consumeGithubOAuthFlow(id: string, binding: string): Promise<GithubOAuthFlow> {
    const flow = this.githubOAuthFlows.get(id);
    if (!flow || flow.binding !== binding) throw new Error('OAuth browser flow is invalid');
    if (flow.consumedAt) throw new StateConflictError('OAuth browser flow was already used');
    if (Date.parse(flow.expiresAt) <= Date.now()) {
      throw new StateConflictError('OAuth browser flow expired');
    }
    const consumed = { ...flow, consumedAt: new Date().toISOString() };
    this.githubOAuthFlows.set(id, consumed);
    return structuredClone(consumed);
  }

  async saveWalletChallenge(challenge: WalletChallenge): Promise<WalletChallenge> {
    this.challenges.set(challenge.id, structuredClone(challenge));
    return challenge;
  }

  async walletChallenge(id: string, githubId: string): Promise<WalletChallenge | undefined> {
    const challenge = this.challenges.get(id);
    return challenge?.githubId === githubId ? structuredClone(challenge) : undefined;
  }

  async consumeWalletChallenge(id: string, githubId: string): Promise<WalletChallenge> {
    const challenge = this.challenges.get(id);
    if (!challenge || challenge.githubId !== githubId)
      throw new Error('wallet challenge not found');
    if (challenge.consumedAt) throw new StateConflictError('wallet challenge already consumed');
    if (Date.parse(challenge.expiresAt) <= Date.now())
      throw new StateConflictError('wallet challenge expired');
    const consumed = { ...challenge, consumedAt: new Date().toISOString() };
    this.challenges.set(id, consumed);
    return structuredClone(consumed);
  }

  async linkContributorWallet(githubId: string, wallet: string): Promise<Contributor> {
    const current = this.contributors.get(githubId);
    if (!current) throw new Error('contributor not found');
    const owner = [...this.contributors.values()].find(
      (candidate) => candidate.githubId !== githubId && candidate.wallet === wallet,
    );
    if (owner) throw new StateConflictError('wallet is already linked to another contributor');
    const contributor = {
      ...current,
      wallet,
      walletVerifiedAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    this.contributors.set(githubId, contributor);
    return structuredClone(contributor);
  }

  async beginWebhookDelivery(deliveryId: string): Promise<WebhookDeliveryLease> {
    const current = this.webhookDeliveries.get(deliveryId);
    if (current?.status === 'completed') return { state: 'completed' };
    if (current?.status === 'processing' && Date.now() - current.updatedAt < 5 * 60_000) {
      return { state: 'busy' };
    }
    const leaseId = randomUUID();
    this.webhookDeliveries.set(deliveryId, {
      status: 'processing',
      leaseId,
      updatedAt: Date.now(),
    });
    return { state: 'started', leaseId };
  }

  async completeWebhookDelivery(deliveryId: string, leaseId: string): Promise<void> {
    const current = this.webhookDeliveries.get(deliveryId);
    if (!current || current.status !== 'processing' || current.leaseId !== leaseId) {
      throw new StateConflictError('webhook delivery lease is no longer active');
    }
    this.webhookDeliveries.set(deliveryId, {
      ...current,
      status: 'completed',
      updatedAt: Date.now(),
    });
  }

  async failWebhookDelivery(deliveryId: string, leaseId: string, _error: string): Promise<void> {
    const current = this.webhookDeliveries.get(deliveryId);
    if (!current || current.status !== 'processing' || current.leaseId !== leaseId) return;
    this.webhookDeliveries.set(deliveryId, {
      ...current,
      status: 'failed',
      updatedAt: Date.now(),
    });
  }

  async createBounty(bounty: RescueBounty): Promise<{ bounty: RescueBounty; created: boolean }> {
    const existing = [...this.bounties.values()].find(
      (candidate) =>
        candidate.sourceJobId === bounty.sourceJobId && candidate.generation === bounty.generation,
    );
    if (existing) return { bounty: structuredClone(existing), created: false };
    this.bounties.set(bounty.id, structuredClone(bounty));
    return { bounty: structuredClone(bounty), created: true };
  }

  async bounty(id: string): Promise<RescueBounty | undefined> {
    return clone(this.bounties.get(id));
  }

  async bountyBySourceJob(jobId: string): Promise<RescueBounty | undefined> {
    return clone(
      [...this.bounties.values()]
        .filter((candidate) => candidate.sourceJobId === jobId)
        .sort((left, right) => right.generation - left.generation)[0],
    );
  }

  async bountiesList(): Promise<RescueBounty[]> {
    return [...this.bounties.values()]
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
      .map((bounty) => structuredClone(bounty));
  }

  async bountiesForAccount(githubId: string, limit: number): Promise<AccountBountiesPage> {
    const boundedLimit = accountBountyLimit(limit);
    const bounties = [...this.bounties.values()]
      .filter(
        (bounty) =>
          bounty.activeClaim?.claimantId === githubId ||
          bounty.claimHistory.some((claim) => claim.claimantId === githubId),
      )
      .sort((left, right) => right.createdAt.localeCompare(left.createdAt))
      .slice(0, boundedLimit + 1)
      .map((bounty) => structuredClone(bounty));
    return {
      bounties: bounties.slice(0, boundedLimit),
      limit: boundedLimit,
      truncated: bounties.length > boundedLimit,
    };
  }

  async updateBounty(bounty: RescueBounty, expectedRevision: number): Promise<RescueBounty> {
    const current = this.bounties.get(bounty.id);
    if (!current) throw new Error(`unknown bounty: ${bounty.id}`);
    if (current.revision !== expectedRevision || bounty.revision !== expectedRevision + 1) {
      throw new StateConflictError(`concurrent update for bounty ${bounty.id}`);
    }
    this.bounties.set(bounty.id, structuredClone(bounty));
    return structuredClone(bounty);
  }

  async saveEscrow(escrow: ContributorEscrow): Promise<ContributorEscrow> {
    const current = this.escrows.get(escrow.id);
    if (!current) {
      if (escrow.revision !== 0) {
        throw new StateConflictError(`escrow ${escrow.id} must start at revision 0`);
      }
      const duplicateClaim = [...this.escrows.values()].find(
        (candidate) => candidate.claimId === escrow.claimId,
      );
      if (duplicateClaim)
        throw new StateConflictError(`escrow already exists for claim ${escrow.claimId}`);
      const active = [...this.escrows.values()].find(
        (candidate) =>
          candidate.bountyId === escrow.bountyId && !terminalEscrowStates.has(candidate.state),
      );
      if (active)
        throw new StateConflictError(`bounty ${escrow.bountyId} already has an active escrow`);
      this.escrows.set(escrow.id, structuredClone(escrow));
      return structuredClone(escrow);
    }
    if (isDeepStrictEqual(current, escrow)) return structuredClone(current);
    if (escrow.revision !== current.revision + 1) {
      throw new StateConflictError(`concurrent update for escrow ${escrow.id}`);
    }
    this.escrows.set(escrow.id, structuredClone(escrow));
    return structuredClone(escrow);
  }

  async escrow(id: string): Promise<ContributorEscrow | undefined> {
    return clone(this.escrows.get(id));
  }

  async escrowByBounty(bountyId: string): Promise<ContributorEscrow | undefined> {
    return clone((await this.escrowsByBounty(bountyId))[0]);
  }

  async escrowsByBounty(bountyId: string): Promise<ContributorEscrow[]> {
    return [...this.escrows.values()]
      .filter((candidate) => candidate.bountyId === bountyId)
      .sort((left, right) => right.createdAt.localeCompare(left.createdAt))
      .map((escrow) => structuredClone(escrow));
  }

  async saveCapability(capability: Capability): Promise<Capability> {
    this.capabilities.set(capability.id, structuredClone(capability));
    return structuredClone(capability);
  }

  async capabilitiesList(): Promise<Capability[]> {
    return [...this.capabilities.values()].map((value) => structuredClone(value));
  }

  async saveUpgrade(upgrade: Upgrade): Promise<Upgrade> {
    this.upgrades.set(upgrade.id, structuredClone(upgrade));
    return structuredClone(upgrade);
  }

  async upgradesList(): Promise<Upgrade[]> {
    return [...this.upgrades.values()].map((value) => structuredClone(value));
  }

  async saveFailure(failure: FailureRecord): Promise<FailureRecord> {
    if (!this.failures.some((candidate) => candidate.id === failure.id)) {
      this.failures.push(structuredClone(failure));
    }
    return structuredClone(failure);
  }

  async failuresForCapability(capabilityKey: string): Promise<FailureRecord[]> {
    return this.failures
      .filter((failure) => failure.capabilityKey === capabilityKey)
      .map((failure) => structuredClone(failure));
  }

  async capabilityByKey(key: string): Promise<Capability | undefined> {
    return clone([...this.capabilities.values()].find((capability) => capability.key === key));
  }

  async close(): Promise<void> {}
}

export class PostgresStore implements MizukiStore {
  private constructor(private readonly pool: Pool) {}

  async readiness(): Promise<void> {
    await this.pool.query('SELECT 1');
  }

  async withAdmissionLock<T>(operation: () => Promise<T>): Promise<T> {
    const client = await this.pool.connect();
    let acquired = false;
    try {
      while (!acquired) {
        const result = await client.query<{ locked: boolean }>(
          "SELECT pg_try_advisory_lock(hashtext('mizuki-commercial-admission')) AS locked",
        );
        acquired = result.rows[0]?.locked === true;
        if (!acquired) await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return await operation();
    } finally {
      try {
        if (acquired) {
          const result = await client.query<{ unlocked: boolean }>(
            "SELECT pg_advisory_unlock(hashtext('mizuki-commercial-admission')) AS unlocked",
          );
          if (!result.rows[0]?.unlocked) throw new Error('commercial admission lock was not held');
        }
      } catch (cause) {
        client.release(cause instanceof Error ? cause : new Error(String(cause)));
        throw cause;
      }
      client.release();
    }
  }

  async operatorControls(): Promise<OperatorControls> {
    const result = await this.pool.query<OperatorControlRow>(
      `SELECT controls.intake_enabled, controls.claims_enabled, controls.revision,
              controls.reason, controls.updated_by, controls.updated_at
       FROM mizuki_operator_controls AS controls
       JOIN LATERAL (
         SELECT intake_enabled, claims_enabled, revision, reason, updated_by, updated_at
         FROM mizuki_operator_control_audit ORDER BY revision DESC LIMIT 1
       ) AS audited ON audited.intake_enabled = controls.intake_enabled
                    AND audited.claims_enabled = controls.claims_enabled
                    AND audited.revision = controls.revision
                    AND audited.reason = controls.reason
                    AND audited.updated_by = controls.updated_by
                    AND audited.updated_at = controls.updated_at
       WHERE controls.singleton = true`,
    );
    const row = result.rows[0];
    if (!row) throw new Error('operator admission controls are unavailable or unaudited');
    return operatorControlsFromRow(row);
  }

  async operatorControlsAudit(): Promise<OperatorControlAuditEntry[]> {
    const result = await this.pool.query<OperatorControlAuditRow>(
      `SELECT expected_revision, intake_enabled, claims_enabled, revision, reason,
              updated_by, updated_at
       FROM mizuki_operator_control_audit ORDER BY revision`,
    );
    return result.rows.map((row) => ({
      ...operatorControlsFromRow(row),
      expectedRevision: Number(row.expected_revision),
    }));
  }

  async updateOperatorControls(patch: OperatorControlsPatch): Promise<OperatorControls> {
    return this.transaction(async (client) => {
      const result = await client.query<OperatorControlRow>(
        `SELECT intake_enabled, claims_enabled, revision, reason, updated_by, updated_at
         FROM mizuki_operator_controls WHERE singleton = true FOR UPDATE`,
      );
      const row = result.rows[0];
      if (!row) throw new Error('operator admission controls are unavailable');
      const audited = await client.query<OperatorControlAuditRow>(
        `SELECT expected_revision, intake_enabled, claims_enabled, revision, reason,
                updated_by, updated_at
         FROM mizuki_operator_control_audit ORDER BY revision DESC LIMIT 1`,
      );
      const current = operatorControlsFromRow(row);
      const latestAudit = audited.rows[0] ? operatorControlsFromRow(audited.rows[0]) : undefined;
      if (!latestAudit || !sameOperatorControls(current, latestAudit)) {
        throw new Error('operator admission controls are unavailable or unaudited');
      }
      const updated = updatedOperatorControls(current, patch);
      await client.query(
        `UPDATE mizuki_operator_controls
         SET intake_enabled = $1, claims_enabled = $2, revision = $3,
             reason = $4, updated_by = $5, updated_at = $6
         WHERE singleton = true`,
        [
          updated.intakeEnabled,
          updated.claimsEnabled,
          updated.revision,
          updated.reason,
          updated.updatedBy,
          updated.updatedAt,
        ],
      );
      await client.query(
        `INSERT INTO mizuki_operator_control_audit (
           revision, expected_revision, intake_enabled, claims_enabled, reason, updated_by,
           updated_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)`,
        [
          updated.revision,
          patch.expectedRevision,
          updated.intakeEnabled,
          updated.claimsEnabled,
          updated.reason,
          updated.updatedBy,
          updated.updatedAt,
        ],
      );
      return updated;
    });
  }

  static async connect(databaseUrl: string): Promise<PostgresStore> {
    const pool = new Pool({
      connectionString: databaseUrl,
      max: 10,
      connectionTimeoutMillis: 5_000,
      query_timeout: 20_000,
      statement_timeout: 15_000,
    });
    const store = new PostgresStore(pool);
    await migrate(pool);
    return store;
  }

  async saveQuote(quote: Quote): Promise<Quote> {
    await this.pool.query(
      `INSERT INTO mizuki_quotes (id, expires_at, payload)
       VALUES ($1, $2, $3::jsonb)
       ON CONFLICT (id) DO UPDATE SET expires_at = EXCLUDED.expires_at, payload = EXCLUDED.payload`,
      [quote.id, quote.expiresAt, JSON.stringify(quote)],
    );
    return quote;
  }

  async quote(id: string): Promise<Quote | undefined> {
    const result = await this.pool.query<{ payload: Quote }>(
      'SELECT payload FROM mizuki_quotes WHERE id = $1',
      [id],
    );
    return result.rows[0]?.payload;
  }

  async linkQuoteToAccount(quoteId: string, githubId: string): Promise<void> {
    const inserted = await this.pool.query<{ github_id: string }>(
      `INSERT INTO mizuki_account_quotes (github_id, quote_id, created_at)
       VALUES ($1, $2, now())
       ON CONFLICT (quote_id) DO NOTHING
       RETURNING github_id`,
      [githubId, quoteId],
    );
    if (inserted.rows[0]) return;
    const current = await this.pool.query<{ github_id: string }>(
      'SELECT github_id FROM mizuki_account_quotes WHERE quote_id = $1',
      [quoteId],
    );
    if (!current.rows[0]) throw new Error('quote or account not found');
    if (current.rows[0].github_id !== githubId) {
      throw new StateConflictError('quote is already linked to another account');
    }
  }

  async quoteForAccount(quoteId: string, githubId: string): Promise<Quote | undefined> {
    const result = await this.pool.query<{ payload: Quote }>(
      `SELECT quotes.payload
       FROM mizuki_quotes AS quotes
       JOIN mizuki_account_quotes AS links ON links.quote_id = quotes.id
       WHERE quotes.id = $1 AND links.github_id = $2`,
      [quoteId, githubId],
    );
    return result.rows[0]?.payload;
  }

  async jobsForAccount(githubId: string, limit: number): Promise<AccountJobsPage> {
    const boundedLimit = accountJobLimit(limit);
    const result = await this.pool.query<{
      payload: Job;
      obligation: boolean;
      id: string;
      created_at: Date;
    }>(
      `WITH account_jobs AS MATERIALIZED (
         SELECT jobs.id, jobs.created_at, jobs.payload,
           jobs.state <> 'refunded' AND (
             jobs.state <> 'delivered' OR (
               jobs.payload ? 'refundLiabilityId' AND
               NOT (jobs.payload ? 'refundLiabilityDischargedAt')
             )
           ) AS obligation
         FROM mizuki_jobs AS jobs
         JOIN mizuki_account_quotes AS links ON links.quote_id = jobs.quote_id
         WHERE links.github_id = $1
       ), obligations AS (
         SELECT id, created_at, payload, true AS obligation
         FROM account_jobs
         WHERE obligation
       ), terminal_history AS (
         SELECT id, created_at, payload, false AS obligation
         FROM account_jobs
         WHERE NOT obligation
         ORDER BY created_at DESC, id DESC
         LIMIT $2
       )
       SELECT id, created_at, payload, obligation
       FROM (
         SELECT * FROM obligations
         UNION ALL
         SELECT * FROM terminal_history
       ) AS selected_jobs
       ORDER BY created_at DESC, id DESC`,
      [githubId, boundedLimit + 1],
    );
    const terminal = result.rows.filter((row) => !row.obligation);
    const includedTerminal = new Set(terminal.slice(0, boundedLimit).map((row) => row.id));
    const jobs = result.rows
      .filter((row) => row.obligation || includedTerminal.has(row.id))
      .map((row) => row.payload);
    return {
      jobs,
      limit: boundedLimit,
      truncated: terminal.length > boundedLimit,
      obligationCount: result.rows.filter((row) => row.obligation).length,
    };
  }

  async linkAccountRepository(
    githubId: string,
    owner: string,
    repo: string,
  ): Promise<AccountRepository> {
    const repository = normalizedRepository(owner, repo);
    const verifiedAt = new Date().toISOString();
    return this.transaction(async (client) => {
      await client.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 0))', [
        `mizuki-account-repositories:${githubId}`,
      ]);
      const capacity = await client.query<{ count: number; existing: boolean | null }>(
        `SELECT count(*)::integer AS count, bool_or(repository = $2) AS existing
         FROM mizuki_account_repositories
         WHERE github_id = $1`,
        [githubId, repository],
      );
      const current = capacity.rows[0];
      if (!current?.existing && (current?.count ?? 0) >= 25) {
        throw new StateConflictError('account repository limit of 25 reached');
      }
      const result = await client.query<{
        github_id: string;
        repository: string;
        owner_name: string;
        repo_name: string;
        verified_at: Date;
      }>(
        `INSERT INTO mizuki_account_repositories
           (github_id, repository, owner_name, repo_name, verified_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (github_id, repository) DO UPDATE
         SET owner_name = EXCLUDED.owner_name,
             repo_name = EXCLUDED.repo_name,
             verified_at = EXCLUDED.verified_at
         RETURNING github_id, repository, owner_name, repo_name, verified_at`,
        [githubId, repository, owner, repo, verifiedAt],
      );
      return accountRepositoryFromRow(result.rows[0]!);
    });
  }

  async repositoriesForAccount(githubId: string, limit: number): Promise<AccountRepositoriesPage> {
    const boundedLimit = accountRepositoryLimit(limit);
    const result = await this.pool.query<{
      github_id: string;
      repository: string;
      owner_name: string;
      repo_name: string;
      verified_at: Date;
    }>(
      `SELECT github_id, repository, owner_name, repo_name, verified_at
       FROM mizuki_account_repositories
       WHERE github_id = $1
       ORDER BY repository
       LIMIT $2`,
      [githubId, boundedLimit + 1],
    );
    return {
      repositories: result.rows.slice(0, boundedLimit).map(accountRepositoryFromRow),
      limit: boundedLimit,
      truncated: result.rows.length > boundedLimit,
    };
  }

  async createJob(
    quote: Quote,
    payment: Payment,
    idempotencyKey: string,
    repositoryAdmission?: RepositoryAdmissionReceipt,
  ): Promise<{ job: Job; created: boolean }> {
    return this.transaction(async (client) => {
      const now = new Date().toISOString();
      const proofHash = paymentProofHash(payment);
      const job: Job = {
        id: randomUUID(),
        idempotencyKey,
        quote,
        payment,
        ...(repositoryAdmission ? { repositoryAdmission } : {}),
        state: 'settlement_pending',
        createdAt: now,
        updatedAt: now,
        inputTokens: 0,
        outputTokens: 0,
        estimatedCostUsd: 0,
        version: 0,
      };
      const inserted = await client.query<{ payload: Job }>(
        `INSERT INTO mizuki_jobs
          (id, idempotency_key, quote_id, payment_proof_hash, state, version,
           created_at, updated_at, payload)
         VALUES ($1, $2, $3, $4, $5, 0, $6, $6, $7::jsonb)
         ON CONFLICT DO NOTHING
         RETURNING payload`,
        [job.id, idempotencyKey, quote.id, proofHash, job.state, now, JSON.stringify(job)],
      );
      if (inserted.rows[0]) return { job: inserted.rows[0].payload, created: true };
      const existing = await client.query<{
        idempotency_key: string;
        quote_id: string;
        payment_proof_hash: string | null;
        payload: Job;
      }>(
        `SELECT idempotency_key, quote_id, payment_proof_hash, payload FROM mizuki_jobs
         WHERE idempotency_key = $1 OR quote_id = $2
            OR ($3::text IS NOT NULL AND payment_proof_hash = $3)
         ORDER BY created_at ASC`,
        [idempotencyKey, quote.id, proofHash],
      );
      const conflicting = existing.rows.find(
        (row) =>
          (row.idempotency_key === idempotencyKey ||
            (proofHash !== undefined && row.payment_proof_hash === proofHash)) &&
          row.quote_id !== quote.id,
      );
      if (conflicting) throw new StateConflictError('payment reservation belongs to another quote');
      const reserved = existing.rows.find((row) => row.quote_id === quote.id);
      if (!reserved) throw new Error('job insert conflicted without an existing row');
      if (
        repositoryAdmission &&
        reserved.payload.repositoryAdmission?.evidenceHash !== repositoryAdmission.evidenceHash
      ) {
        throw new StateConflictError('job reservation has different repository admission');
      }
      return { job: reserved.payload, created: false };
    });
  }

  async job(id: string): Promise<Job | undefined> {
    const result = await this.pool.query<{ payload: Job }>(
      'SELECT payload FROM mizuki_jobs WHERE id = $1',
      [id],
    );
    return result.rows[0]?.payload;
  }

  async jobByIdempotencyKey(key: string): Promise<Job | undefined> {
    const result = await this.pool.query<{ payload: Job }>(
      'SELECT payload FROM mizuki_jobs WHERE idempotency_key = $1',
      [key],
    );
    return result.rows[0]?.payload;
  }

  async jobByQuote(quoteId: string): Promise<Job | undefined> {
    const result = await this.pool.query<{ payload: Job }>(
      'SELECT payload FROM mizuki_jobs WHERE quote_id = $1',
      [quoteId],
    );
    return result.rows[0]?.payload;
  }

  async jobsList(): Promise<Job[]> {
    const result = await this.pool.query<{ payload: Job }>(
      'SELECT payload FROM mizuki_jobs ORDER BY created_at DESC',
    );
    return result.rows.map((row) => row.payload);
  }

  async transitionJob(
    id: string,
    expected: JobState | readonly JobState[],
    state: JobState,
    patch: JobPatch = {},
  ): Promise<Job> {
    return this.transaction(async (client) => {
      const current = await lockedJob(client, id);
      const allowed = Array.isArray(expected) ? expected : [expected];
      if (!allowed.includes(current.state)) {
        throw new StateConflictError(
          `job ${id} is ${current.state}; expected ${allowed.join(', ')}`,
        );
      }
      const job = updateJob(current, state, patch);
      await writeJob(client, job);
      return job;
    });
  }

  async patchJob(id: string, patch: JobPatch): Promise<Job> {
    return this.transaction(async (client) => {
      const current = await lockedJob(client, id);
      const job = updateJob(current, current.state, patch);
      await writeJob(client, job);
      return job;
    });
  }

  async appendLedger(entry: Omit<LedgerEntry, 'id' | 'createdAt'>): Promise<LedgerEntry> {
    const stored = { ...entry, id: randomUUID(), createdAt: new Date().toISOString() };
    const inserted = await this.pool.query<{ payload: LedgerEntry }>(
      `INSERT INTO mizuki_ledger
        (id, kind, reference_id, asset, amount_atomic, amount_usd, transaction, created_at, payload)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb)
       ON CONFLICT (kind, reference_id) DO NOTHING
       RETURNING payload`,
      [
        stored.id,
        stored.kind,
        stored.referenceId,
        stored.asset,
        stored.amountAtomic,
        stored.amountUsd,
        stored.transaction ?? null,
        stored.createdAt,
        JSON.stringify(stored),
      ],
    );
    if (inserted.rows[0]) return inserted.rows[0].payload;
    const existing = await this.pool.query<{ payload: LedgerEntry }>(
      'SELECT payload FROM mizuki_ledger WHERE kind = $1 AND reference_id = $2',
      [entry.kind, entry.referenceId],
    );
    const value = existing.rows[0]?.payload;
    if (!value) throw new Error('ledger insert conflicted without an existing row');
    if (
      value.asset !== entry.asset ||
      value.amountAtomic !== entry.amountAtomic ||
      Number(value.amountUsd) !== entry.amountUsd ||
      value.transaction !== entry.transaction
    ) {
      throw new StateConflictError('ledger idempotency key reused with different values');
    }
    return value;
  }

  async ledgerEntries(): Promise<LedgerEntry[]> {
    const result = await this.pool.query<{ payload: LedgerEntry }>(
      'SELECT payload FROM mizuki_ledger ORDER BY created_at DESC',
    );
    return result.rows.map((row) => row.payload);
  }

  async appendActivity(
    kind: ActivityKind,
    subjectId: string,
    publicData: Record<string, unknown> = {},
    eventId = randomUUID(),
  ): Promise<ActivityEvent> {
    const event = {
      id: eventId,
      kind,
      subjectId,
      publicData,
      createdAt: new Date().toISOString(),
    };
    const result = await this.pool.query<{ payload: ActivityEvent }>(
      `INSERT INTO mizuki_activity (id, kind, subject_id, created_at, payload)
       VALUES ($1, $2, $3, $4, $5::jsonb)
       ON CONFLICT (id) DO NOTHING
       RETURNING payload`,
      [event.id, kind, subjectId, event.createdAt, JSON.stringify(event)],
    );
    if (result.rows[0]) return result.rows[0].payload;
    const existing = await this.pool.query<{ payload: ActivityEvent }>(
      'SELECT payload FROM mizuki_activity WHERE id = $1',
      [event.id],
    );
    const stored = existing.rows[0]?.payload;
    if (
      !stored ||
      stored.kind !== kind ||
      stored.subjectId !== subjectId ||
      !isDeepStrictEqual(stored.publicData, publicData)
    ) {
      throw new StateConflictError('activity event id reused with different values');
    }
    return stored;
  }

  async activity(limit = 100): Promise<ActivityEvent[]> {
    const result = await this.pool.query<{ payload: ActivityEvent }>(
      'SELECT payload FROM mizuki_activity ORDER BY created_at DESC LIMIT $1',
      [Math.min(Math.max(limit, 1), 500)],
    );
    return result.rows.map((row) => row.payload);
  }

  async upsertContributor(githubId: string, githubLogin: string): Promise<Contributor> {
    const now = new Date().toISOString();
    const result = await this.pool.query<{ payload: Contributor }>(
      `INSERT INTO mizuki_contributors (github_id, github_login, created_at, updated_at, payload)
       VALUES ($1, $2, $3, $3, $4::jsonb)
       ON CONFLICT (github_id) DO UPDATE
       SET github_login = EXCLUDED.github_login,
           updated_at = EXCLUDED.updated_at,
           payload = mizuki_contributors.payload || jsonb_build_object(
             'githubLogin', EXCLUDED.github_login,
             'updatedAt', EXCLUDED.updated_at
           )
       RETURNING payload`,
      [
        githubId,
        githubLogin,
        now,
        JSON.stringify({ githubId, githubLogin, createdAt: now, updatedAt: now }),
      ],
    );
    return result.rows[0]!.payload;
  }

  async contributor(githubId: string): Promise<Contributor | undefined> {
    const result = await this.pool.query<{ payload: Contributor }>(
      'SELECT payload FROM mizuki_contributors WHERE github_id = $1',
      [githubId],
    );
    return result.rows[0]?.payload;
  }

  async saveGithubOAuthFlow(flow: GithubOAuthFlow): Promise<GithubOAuthFlow> {
    await this.transaction(async (client) => {
      await client.query(
        "SELECT pg_advisory_xact_lock(hashtext('mizuki-github-oauth-flow-admission'))",
      );
      await client.query(
        `DELETE FROM mizuki_github_oauth_flows
         WHERE consumed_at IS NOT NULL OR expires_at <= now()`,
      );
      const count = await client.query<{ count: string }>(
        'SELECT count(*)::text AS count FROM mizuki_github_oauth_flows',
      );
      if (Number(count.rows[0]?.count ?? 0) >= MAX_PENDING_GITHUB_OAUTH_FLOWS) {
        throw new GithubOAuthCapacityError();
      }
      await client.query(
        `INSERT INTO mizuki_github_oauth_flows
          (id, binding, expires_at, created_at)
         VALUES ($1, $2, $3, $4)`,
        [flow.id, flow.binding, flow.expiresAt, flow.createdAt],
      );
    });
    return flow;
  }

  async consumeGithubOAuthFlow(id: string, binding: string): Promise<GithubOAuthFlow> {
    return this.transaction(async (client) => {
      const result = await client.query<{
        id: string;
        binding: string;
        expires_at: Date;
        created_at: Date;
        consumed_at: Date;
      }>(
        `UPDATE mizuki_github_oauth_flows
         SET consumed_at = now()
         WHERE id = $1 AND binding = $2 AND consumed_at IS NULL AND expires_at > now()
         RETURNING id, binding, expires_at, created_at, consumed_at`,
        [id, binding],
      );
      const row = result.rows[0];
      if (row) {
        return {
          id: row.id,
          binding: row.binding,
          expiresAt: row.expires_at.toISOString(),
          createdAt: row.created_at.toISOString(),
          consumedAt: row.consumed_at.toISOString(),
        };
      }

      const current = await client.query<{
        binding: string;
        expires_at: Date;
        consumed_at: Date | null;
      }>(
        `SELECT binding, expires_at, consumed_at
         FROM mizuki_github_oauth_flows WHERE id = $1`,
        [id],
      );
      const existing = current.rows[0];
      if (!existing || existing.binding !== binding) {
        throw new Error('OAuth browser flow is invalid');
      }
      if (existing.consumed_at) {
        throw new StateConflictError('OAuth browser flow was already used');
      }
      throw new StateConflictError('OAuth browser flow expired');
    });
  }

  async saveWalletChallenge(challenge: WalletChallenge): Promise<WalletChallenge> {
    await this.pool.query(
      `INSERT INTO mizuki_wallet_challenges
        (id, github_id, wallet, expires_at, created_at, payload)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb)`,
      [
        challenge.id,
        challenge.githubId,
        challenge.wallet,
        challenge.expiresAt,
        challenge.createdAt,
        JSON.stringify(challenge),
      ],
    );
    return challenge;
  }

  async walletChallenge(id: string, githubId: string): Promise<WalletChallenge | undefined> {
    const result = await this.pool.query<{ payload: WalletChallenge }>(
      `SELECT payload FROM mizuki_wallet_challenges WHERE id = $1 AND github_id = $2`,
      [id, githubId],
    );
    return result.rows[0]?.payload;
  }

  async consumeWalletChallenge(id: string, githubId: string): Promise<WalletChallenge> {
    return this.transaction(async (client) => {
      const result = await client.query<{ payload: WalletChallenge }>(
        `SELECT payload FROM mizuki_wallet_challenges
         WHERE id = $1 AND github_id = $2 FOR UPDATE`,
        [id, githubId],
      );
      const challenge = result.rows[0]?.payload;
      if (!challenge) throw new Error('wallet challenge not found');
      if (challenge.consumedAt) throw new StateConflictError('wallet challenge already consumed');
      if (Date.parse(challenge.expiresAt) <= Date.now())
        throw new StateConflictError('wallet challenge expired');
      const consumed = { ...challenge, consumedAt: new Date().toISOString() };
      await client.query(
        `UPDATE mizuki_wallet_challenges
         SET consumed_at = $2, payload = $3::jsonb WHERE id = $1`,
        [id, consumed.consumedAt, JSON.stringify(consumed)],
      );
      return consumed;
    });
  }

  async linkContributorWallet(githubId: string, wallet: string): Promise<Contributor> {
    const current = await this.contributor(githubId);
    if (!current) throw new Error('contributor not found');
    const contributor = {
      ...current,
      wallet,
      walletVerifiedAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    await this.pool.query(
      `UPDATE mizuki_contributors
       SET wallet = $2, updated_at = $3, payload = $4::jsonb
       WHERE github_id = $1`,
      [githubId, wallet, contributor.updatedAt, JSON.stringify(contributor)],
    );
    return contributor;
  }

  async beginWebhookDelivery(deliveryId: string): Promise<WebhookDeliveryLease> {
    const leaseId = randomUUID();
    const result = await this.pool.query<{ lease_id: string }>(
      `INSERT INTO mizuki_webhook_deliveries
         (delivery_id, status, attempts, lease_id, received_at, updated_at)
       VALUES ($1, 'processing', 1, $2, now(), now())
       ON CONFLICT (delivery_id) DO UPDATE
       SET status = 'processing', attempts = mizuki_webhook_deliveries.attempts + 1,
           lease_id = EXCLUDED.lease_id, updated_at = now(), last_error = NULL
       WHERE mizuki_webhook_deliveries.status = 'failed'
          OR (mizuki_webhook_deliveries.status = 'processing'
              AND mizuki_webhook_deliveries.updated_at < now() - interval '5 minutes')
       RETURNING lease_id`,
      [deliveryId, leaseId],
    );
    if (result.rows[0]) return { state: 'started', leaseId: result.rows[0].lease_id };
    const current = await this.pool.query<{ status: string }>(
      `SELECT status FROM mizuki_webhook_deliveries WHERE delivery_id = $1`,
      [deliveryId],
    );
    return { state: current.rows[0]?.status === 'completed' ? 'completed' : 'busy' };
  }

  async completeWebhookDelivery(deliveryId: string, leaseId: string): Promise<void> {
    const result = await this.pool.query(
      `UPDATE mizuki_webhook_deliveries
       SET status = 'completed', updated_at = now(), last_error = NULL
       WHERE delivery_id = $1 AND lease_id = $2 AND status = 'processing'`,
      [deliveryId, leaseId],
    );
    if (result.rowCount !== 1) {
      throw new StateConflictError('webhook delivery lease is no longer active');
    }
  }

  async failWebhookDelivery(deliveryId: string, leaseId: string, error: string): Promise<void> {
    await this.pool.query(
      `UPDATE mizuki_webhook_deliveries
       SET status = 'failed', updated_at = now(), last_error = $3
       WHERE delivery_id = $1 AND lease_id = $2 AND status = 'processing'`,
      [deliveryId, leaseId, error.slice(0, 2_000)],
    );
  }

  async createBounty(bounty: RescueBounty): Promise<{ bounty: RescueBounty; created: boolean }> {
    const result = await this.pool.query<{ payload: RescueBounty }>(
      `INSERT INTO mizuki_bounties
        (id, source_job_id, generation, state, revision, created_at, updated_at, payload)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb)
       ON CONFLICT (source_job_id, generation) DO NOTHING
       RETURNING payload`,
      [
        bounty.id,
        bounty.sourceJobId,
        bounty.generation,
        bounty.state,
        bounty.revision,
        bounty.createdAt,
        bounty.updatedAt,
        JSON.stringify(bounty),
      ],
    );
    if (result.rows[0]) return { bounty: result.rows[0].payload, created: true };
    const existingResult = await this.pool.query<{ payload: RescueBounty }>(
      `SELECT payload FROM mizuki_bounties WHERE source_job_id = $1 AND generation = $2`,
      [bounty.sourceJobId, bounty.generation],
    );
    const existing = existingResult.rows[0]?.payload;
    if (!existing) throw new Error('bounty insert conflicted without an existing row');
    return { bounty: existing, created: false };
  }

  async bounty(id: string): Promise<RescueBounty | undefined> {
    const result = await this.pool.query<{ payload: RescueBounty }>(
      'SELECT payload FROM mizuki_bounties WHERE id = $1',
      [id],
    );
    return result.rows[0]?.payload;
  }

  async bountyBySourceJob(jobId: string): Promise<RescueBounty | undefined> {
    const result = await this.pool.query<{ payload: RescueBounty }>(
      'SELECT payload FROM mizuki_bounties WHERE source_job_id = $1 ORDER BY generation DESC LIMIT 1',
      [jobId],
    );
    return result.rows[0]?.payload;
  }

  async bountiesList(): Promise<RescueBounty[]> {
    const result = await this.pool.query<{ payload: RescueBounty }>(
      'SELECT payload FROM mizuki_bounties ORDER BY created_at DESC',
    );
    return result.rows.map((row) => row.payload);
  }

  async bountiesForAccount(githubId: string, limit: number): Promise<AccountBountiesPage> {
    const boundedLimit = accountBountyLimit(limit);
    const result = await this.pool.query<{ payload: RescueBounty }>(
      `SELECT payload
       FROM mizuki_bounties
       WHERE payload->'activeClaim'->>'claimantId' = $1
          OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements(COALESCE(payload->'claimHistory', '[]'::jsonb)) AS claim
            WHERE claim->>'claimantId' = $1
          )
       ORDER BY created_at DESC
       LIMIT $2`,
      [githubId, boundedLimit + 1],
    );
    return {
      bounties: result.rows.slice(0, boundedLimit).map((row) => row.payload),
      limit: boundedLimit,
      truncated: result.rows.length > boundedLimit,
    };
  }

  async updateBounty(bounty: RescueBounty, expectedRevision: number): Promise<RescueBounty> {
    if (bounty.revision !== expectedRevision + 1) {
      throw new StateConflictError(`invalid revision for bounty ${bounty.id}`);
    }
    const result = await this.pool.query(
      `UPDATE mizuki_bounties
       SET state = $2, revision = $3, updated_at = $4, payload = $5::jsonb
       WHERE id = $1 AND revision = $6`,
      [
        bounty.id,
        bounty.state,
        bounty.revision,
        bounty.updatedAt,
        JSON.stringify(bounty),
        expectedRevision,
      ],
    );
    if (result.rowCount !== 1)
      throw new StateConflictError(`concurrent update for bounty ${bounty.id}`);
    return bounty;
  }

  async saveEscrow(escrow: ContributorEscrow): Promise<ContributorEscrow> {
    const values = [
      escrow.id,
      escrow.bountyId,
      escrow.state,
      escrow.revision,
      escrow.createdAt,
      escrow.updatedAt,
      JSON.stringify(escrow),
    ];
    const result =
      escrow.revision === 0
        ? await this.pool.query<{ payload: ContributorEscrow }>(
            `INSERT INTO mizuki_contributor_escrows
            (id, bounty_id, state, revision, created_at, updated_at, payload)
           VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
           ON CONFLICT DO NOTHING RETURNING payload`,
            values,
          )
        : await this.pool.query<{ payload: ContributorEscrow }>(
            `UPDATE mizuki_contributor_escrows
           SET state = $2, revision = $3, updated_at = $4, payload = $5::jsonb
           WHERE id = $1 AND revision = $3 - 1
           RETURNING payload`,
            [escrow.id, escrow.state, escrow.revision, escrow.updatedAt, JSON.stringify(escrow)],
          );
    if (result.rows[0]) return result.rows[0].payload;

    const existing = await this.escrow(escrow.id);
    if (existing && isDeepStrictEqual(existing, escrow)) return existing;
    throw new StateConflictError(`concurrent update for escrow ${escrow.id}`);
  }

  async escrow(id: string): Promise<ContributorEscrow | undefined> {
    const result = await this.pool.query<{ payload: ContributorEscrow }>(
      'SELECT payload FROM mizuki_contributor_escrows WHERE id = $1',
      [id],
    );
    return result.rows[0]?.payload;
  }

  async escrowByBounty(bountyId: string): Promise<ContributorEscrow | undefined> {
    return (await this.escrowsByBounty(bountyId))[0];
  }

  async escrowsByBounty(bountyId: string): Promise<ContributorEscrow[]> {
    const result = await this.pool.query<{ payload: ContributorEscrow }>(
      'SELECT payload FROM mizuki_contributor_escrows WHERE bounty_id = $1 ORDER BY created_at DESC, id DESC',
      [bountyId],
    );
    return result.rows.map((row) => row.payload);
  }

  async saveCapability(capability: Capability): Promise<Capability> {
    await saveVersioned(this.pool, 'mizuki_capabilities', capability);
    return capability;
  }

  async capabilitiesList(): Promise<Capability[]> {
    const result = await this.pool.query<{ payload: Capability }>(
      'SELECT payload FROM mizuki_capabilities ORDER BY created_at ASC',
    );
    return result.rows.map((row) => row.payload);
  }

  async saveUpgrade(upgrade: Upgrade): Promise<Upgrade> {
    await saveVersioned(this.pool, 'mizuki_upgrades', upgrade);
    return upgrade;
  }

  async upgradesList(): Promise<Upgrade[]> {
    const result = await this.pool.query<{ payload: Upgrade }>(
      'SELECT payload FROM mizuki_upgrades ORDER BY created_at DESC',
    );
    return result.rows.map((row) => row.payload);
  }

  async saveFailure(failure: FailureRecord): Promise<FailureRecord> {
    await this.pool.query(
      `INSERT INTO mizuki_failures (id, capability_key, occurred_at, payload)
       VALUES ($1, $2, $3, $4::jsonb) ON CONFLICT (id) DO NOTHING`,
      [failure.id, failure.capabilityKey, failure.occurredAt, JSON.stringify(failure)],
    );
    return failure;
  }

  async failuresForCapability(capabilityKey: string): Promise<FailureRecord[]> {
    const result = await this.pool.query<{ payload: FailureRecord }>(
      `SELECT payload FROM mizuki_failures
       WHERE capability_key = $1 ORDER BY occurred_at DESC`,
      [capabilityKey],
    );
    return result.rows.map((row) => row.payload);
  }

  async capabilityByKey(key: string): Promise<Capability | undefined> {
    const result = await this.pool.query<{ payload: Capability }>(
      `SELECT payload FROM mizuki_capabilities WHERE payload->>'key' = $1 LIMIT 1`,
      [key],
    );
    return result.rows[0]?.payload;
  }

  async close(): Promise<void> {
    await this.pool.end();
  }

  private async transaction<T>(run: (client: PoolClient) => Promise<T>): Promise<T> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const value = await run(client);
      await client.query('COMMIT');
      return value;
    } catch (cause) {
      await client.query('ROLLBACK');
      throw cause;
    } finally {
      client.release();
    }
  }
}

export class StateConflictError extends Error {}

export class GithubOAuthCapacityError extends Error {
  constructor() {
    super('GitHub sign-in is temporarily busy; try again shortly');
  }
}

const terminalEscrowStates = new Set<ContributorEscrow['state']>([
  'released',
  'refunded',
  'failed',
]);

async function lockedJob(client: PoolClient, id: string): Promise<Job> {
  const result = await client.query<{ payload: Job }>(
    'SELECT payload FROM mizuki_jobs WHERE id = $1 FOR UPDATE',
    [id],
  );
  const job = result.rows[0]?.payload;
  if (!job) throw new Error(`unknown job: ${id}`);
  return job;
}

async function writeJob(client: PoolClient, job: Job): Promise<void> {
  const result = await client.query(
    `UPDATE mizuki_jobs
     SET state = $2, version = $3, updated_at = $4, settlement_transaction = $5,
         payload = $6::jsonb
     WHERE id = $1 AND version = $7`,
    [
      job.id,
      job.state,
      job.version,
      job.updatedAt,
      job.payment.transaction === 'pending' ? null : job.payment.transaction,
      JSON.stringify(job),
      job.version - 1,
    ],
  );
  if (result.rowCount !== 1) throw new StateConflictError(`concurrent update for job ${job.id}`);
}

function updateJob(current: Job, state: JobState, patch: JobPatch): Job {
  return {
    ...current,
    ...patch,
    id: current.id,
    state,
    version: current.version + 1,
    createdAt: current.createdAt,
    updatedAt: new Date().toISOString(),
  };
}

function clone<T>(value: T | undefined): T | undefined {
  return value === undefined ? undefined : structuredClone(value);
}

type OperatorControlRow = {
  intake_enabled: boolean;
  claims_enabled: boolean;
  revision: number;
  reason: string;
  updated_by: string;
  updated_at: Date | string;
};

type OperatorControlAuditRow = OperatorControlRow & {
  expected_revision: number;
};

function initialOperatorControls(): OperatorControls {
  return {
    intakeEnabled: false,
    claimsEnabled: false,
    revision: 0,
    reason: 'initial deployment: closed',
    updatedBy: 'system',
    updatedAt: new Date().toISOString(),
  };
}

function updatedOperatorControls(
  current: OperatorControls,
  patch: OperatorControlsPatch,
): OperatorControls {
  const opensAdmission =
    (patch.intakeEnabled === true && !current.intakeEnabled) ||
    (patch.claimsEnabled === true && !current.claimsEnabled);
  if (
    patch.expectedRevision > current.revision ||
    (patch.expectedRevision < current.revision && opensAdmission)
  ) {
    throw new StateConflictError(
      `expected operator admission revision ${patch.expectedRevision}; current revision is ${current.revision}`,
    );
  }
  return {
    intakeEnabled: patch.intakeEnabled ?? current.intakeEnabled,
    claimsEnabled: patch.claimsEnabled ?? current.claimsEnabled,
    revision: current.revision + 1,
    reason: patch.reason,
    updatedBy: patch.updatedBy,
    updatedAt: new Date().toISOString(),
  };
}

function sameOperatorControls(left: OperatorControls, right: OperatorControls): boolean {
  return (
    left.intakeEnabled === right.intakeEnabled &&
    left.claimsEnabled === right.claimsEnabled &&
    left.revision === right.revision &&
    left.reason === right.reason &&
    left.updatedBy === right.updatedBy &&
    left.updatedAt === right.updatedAt
  );
}

function operatorControlsFromRow(row: OperatorControlRow): OperatorControls {
  return {
    intakeEnabled: row.intake_enabled,
    claimsEnabled: row.claims_enabled,
    revision: Number(row.revision),
    reason: row.reason,
    updatedBy: row.updated_by,
    updatedAt: new Date(row.updated_at).toISOString(),
  };
}

function paymentProofHash(payment: Payment): string | undefined {
  return payment.signature
    ? createHash('sha256').update(payment.signature).digest('hex')
    : undefined;
}

function normalizedRepository(owner: string, repo: string): string {
  const segment = /^[A-Za-z0-9_.-]{1,100}$/;
  if (!segment.test(owner) || !segment.test(repo)) {
    throw new Error('repository identity is invalid');
  }
  return `${owner}/${repo}`.toLowerCase();
}

function accountJobLimit(limit: number): number {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 1_000) {
    throw new Error('account job limit must be an integer between 1 and 1000');
  }
  return limit;
}

function accountJobIsObligation(job: Job): boolean {
  if (job.state === 'refunded') return false;
  if (job.state !== 'delivered') return true;
  return Boolean(job.refundLiabilityId && !job.refundLiabilityDischargedAt);
}

function accountJobOrder(left: Job, right: Job): number {
  return right.createdAt.localeCompare(left.createdAt) || right.id.localeCompare(left.id);
}

function accountRepositoryLimit(limit: number): number {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 25) {
    throw new Error('account repository limit must be an integer between 1 and 25');
  }
  return limit;
}

function accountBountyLimit(limit: number): number {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
    throw new Error('account bounty limit must be an integer between 1 and 100');
  }
  return limit;
}

function accountRepositoryFromRow(row: {
  github_id: string;
  repository: string;
  owner_name: string;
  repo_name: string;
  verified_at: Date;
}): AccountRepository {
  return {
    githubId: row.github_id,
    owner: row.owner_name,
    repo: row.repo_name,
    repository: row.repository,
    verifiedAt: new Date(row.verified_at).toISOString(),
  };
}

async function saveVersioned(
  pool: Pool,
  table: 'mizuki_capabilities' | 'mizuki_upgrades',
  value: Capability | Upgrade,
): Promise<void> {
  await pool.query(
    `INSERT INTO ${table} (id, state, revision, created_at, updated_at, payload)
     VALUES ($1, $2, $3, $4, $5, $6::jsonb)
     ON CONFLICT (id) DO UPDATE
     SET state = EXCLUDED.state, revision = EXCLUDED.revision,
         updated_at = EXCLUDED.updated_at, payload = EXCLUDED.payload
     WHERE ${table}.revision < EXCLUDED.revision`,
    [
      value.id,
      value.state,
      value.revision,
      value.createdAt,
      value.updatedAt,
      JSON.stringify(value),
    ],
  );
}

export const COMMERCIAL_CORE_SCHEMA_V1 = `
CREATE TABLE IF NOT EXISTS mizuki_operator_controls (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  intake_enabled boolean NOT NULL,
  claims_enabled boolean NOT NULL,
  revision integer NOT NULL,
  reason text NOT NULL,
  updated_by text NOT NULL,
  updated_at timestamptz NOT NULL
);
INSERT INTO mizuki_operator_controls
  (singleton, intake_enabled, claims_enabled, revision, reason, updated_by, updated_at)
VALUES (true, false, false, 0, 'initial deployment: closed', 'system', now())
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS mizuki_quotes (
  id uuid PRIMARY KEY,
  expires_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);

CREATE TABLE IF NOT EXISTS mizuki_jobs (
  id uuid PRIMARY KEY,
  idempotency_key text NOT NULL UNIQUE,
  quote_id uuid NOT NULL REFERENCES mizuki_quotes(id),
  payment_proof_hash text,
  settlement_transaction text,
  state text NOT NULL,
  version integer NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
ALTER TABLE mizuki_jobs ADD COLUMN IF NOT EXISTS payment_proof_hash text;
ALTER TABLE mizuki_jobs ADD COLUMN IF NOT EXISTS settlement_transaction text;
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_jobs_payment_proof_idx
  ON mizuki_jobs(payment_proof_hash) WHERE payment_proof_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_jobs_settlement_transaction_idx
  ON mizuki_jobs(settlement_transaction) WHERE settlement_transaction IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_jobs_quote_idx ON mizuki_jobs(quote_id);
CREATE INDEX IF NOT EXISTS mizuki_jobs_state_idx ON mizuki_jobs(state);
CREATE INDEX IF NOT EXISTS mizuki_jobs_created_idx ON mizuki_jobs(created_at DESC);

CREATE TABLE IF NOT EXISTS mizuki_ledger (
  id uuid PRIMARY KEY,
  kind text NOT NULL,
  reference_id text NOT NULL,
  asset text NOT NULL,
  amount_atomic numeric(40, 0) NOT NULL,
  amount_usd numeric(20, 8) NOT NULL,
  transaction text,
  created_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS mizuki_ledger_reference_idx ON mizuki_ledger(reference_id);
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_ledger_idempotency_idx
  ON mizuki_ledger(kind, reference_id);

CREATE TABLE IF NOT EXISTS mizuki_activity (
  id uuid PRIMARY KEY,
  kind text NOT NULL,
  subject_id text NOT NULL,
  created_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS mizuki_activity_created_idx ON mizuki_activity(created_at DESC);

CREATE TABLE IF NOT EXISTS mizuki_contributors (
  github_id text PRIMARY KEY,
  github_login text NOT NULL,
  wallet text UNIQUE,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);

CREATE TABLE IF NOT EXISTS mizuki_wallet_challenges (
  id uuid PRIMARY KEY,
  github_id text NOT NULL REFERENCES mizuki_contributors(github_id),
  wallet text NOT NULL,
  expires_at timestamptz NOT NULL,
  consumed_at timestamptz,
  created_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);

CREATE TABLE IF NOT EXISTS mizuki_webhook_deliveries (
  delivery_id text PRIMARY KEY,
  status text NOT NULL DEFAULT 'processing',
  attempts integer NOT NULL DEFAULT 1,
  lease_id uuid,
  received_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL DEFAULT now(),
  last_error text
);
ALTER TABLE mizuki_webhook_deliveries ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'processing';
ALTER TABLE mizuki_webhook_deliveries ADD COLUMN IF NOT EXISTS attempts integer NOT NULL DEFAULT 1;
ALTER TABLE mizuki_webhook_deliveries ADD COLUMN IF NOT EXISTS lease_id uuid;
ALTER TABLE mizuki_webhook_deliveries ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE mizuki_webhook_deliveries ADD COLUMN IF NOT EXISTS last_error text;

CREATE TABLE IF NOT EXISTS mizuki_bounties (
  id uuid PRIMARY KEY,
  source_job_id uuid NOT NULL REFERENCES mizuki_jobs(id),
  generation integer NOT NULL DEFAULT 0,
  state text NOT NULL,
  revision integer NOT NULL,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
ALTER TABLE mizuki_bounties DROP CONSTRAINT IF EXISTS mizuki_bounties_source_job_id_key;
ALTER TABLE mizuki_bounties ADD COLUMN IF NOT EXISTS generation integer NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_bounties_source_generation_idx
  ON mizuki_bounties(source_job_id, generation);
CREATE INDEX IF NOT EXISTS mizuki_bounties_state_idx ON mizuki_bounties(state);

CREATE TABLE IF NOT EXISTS mizuki_contributor_escrows (
  id uuid PRIMARY KEY,
  bounty_id uuid NOT NULL REFERENCES mizuki_bounties(id),
  state text NOT NULL,
  revision integer NOT NULL,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_contributor_escrows_claim_idx
  ON mizuki_contributor_escrows ((payload->>'claimId'))
  WHERE payload->>'claimId' IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_contributor_escrows_active_bounty_idx
  ON mizuki_contributor_escrows (bounty_id)
  WHERE state NOT IN ('released', 'refunded', 'failed');

CREATE TABLE IF NOT EXISTS mizuki_capabilities (
  id uuid PRIMARY KEY,
  state text NOT NULL,
  revision integer NOT NULL,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS mizuki_capabilities_key_idx
  ON mizuki_capabilities ((payload->>'key'));

CREATE TABLE IF NOT EXISTS mizuki_upgrades (
  id uuid PRIMARY KEY,
  state text NOT NULL,
  revision integer NOT NULL,
  created_at timestamptz NOT NULL,
  updated_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);

CREATE TABLE IF NOT EXISTS mizuki_failures (
  id uuid PRIMARY KEY,
  capability_key text NOT NULL,
  occurred_at timestamptz NOT NULL,
  payload jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS mizuki_failures_capability_idx
  ON mizuki_failures(capability_key, occurred_at DESC);
`;

export const WORKBENCH_ACCOUNTS_SCHEMA_V1 = `
CREATE TABLE mizuki_account_quotes (
  github_id text NOT NULL REFERENCES mizuki_contributors(github_id),
  quote_id uuid NOT NULL UNIQUE REFERENCES mizuki_quotes(id),
  created_at timestamptz NOT NULL,
  PRIMARY KEY (github_id, quote_id)
);
CREATE INDEX mizuki_account_quotes_github_idx
  ON mizuki_account_quotes(github_id, created_at DESC);

CREATE TABLE mizuki_account_repositories (
  github_id text NOT NULL REFERENCES mizuki_contributors(github_id),
  repository text NOT NULL CHECK (
    repository ~ '^[a-z0-9_.-]+/[a-z0-9_.-]+$'
  ),
  owner_name text NOT NULL CHECK (owner_name ~ '^[A-Za-z0-9_.-]{1,100}$'),
  repo_name text NOT NULL CHECK (repo_name ~ '^[A-Za-z0-9_.-]{1,100}$'),
  verified_at timestamptz NOT NULL,
  PRIMARY KEY (github_id, repository)
);
CREATE INDEX mizuki_account_repositories_verified_idx
  ON mizuki_account_repositories(github_id, verified_at DESC);
`;

export const GITHUB_OAUTH_FLOW_SCHEMA_V1 = `
CREATE TABLE mizuki_github_oauth_flows (
  id uuid PRIMARY KEY,
  binding text NOT NULL CHECK (binding ~ '^[A-Za-z0-9_-]{43}$'),
  expires_at timestamptz NOT NULL,
  consumed_at timestamptz,
  created_at timestamptz NOT NULL
);
CREATE INDEX mizuki_github_oauth_flows_expiry_idx
  ON mizuki_github_oauth_flows(expires_at);
`;

const ADMISSION_CONTROL_AUDIT_SCHEMA = `
CREATE TABLE mizuki_operator_control_audit (
  revision integer PRIMARY KEY CHECK (revision >= 0),
  expected_revision integer NOT NULL CHECK (
    expected_revision >= 0 AND expected_revision <= revision
  ),
  intake_enabled boolean NOT NULL,
  claims_enabled boolean NOT NULL,
  reason text NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 500),
  updated_by text NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 128),
  updated_at timestamptz NOT NULL
);
INSERT INTO mizuki_operator_control_audit (
  revision, expected_revision, intake_enabled, claims_enabled, reason, updated_by, updated_at
)
SELECT revision, revision, intake_enabled, claims_enabled, reason, updated_by, updated_at
FROM mizuki_operator_controls WHERE singleton = true;
CREATE FUNCTION mizuki_reject_operator_control_audit_mutation() RETURNS trigger
  LANGUAGE plpgsql AS $$
  BEGIN
    RAISE EXCEPTION 'operator control audit is append-only';
  END;
  $$;
CREATE TRIGGER mizuki_operator_control_audit_append_only
  BEFORE UPDATE OR DELETE ON mizuki_operator_control_audit
  FOR EACH ROW EXECUTE FUNCTION mizuki_reject_operator_control_audit_mutation();
CREATE TRIGGER mizuki_operator_control_audit_no_truncate
  BEFORE TRUNCATE ON mizuki_operator_control_audit
  FOR EACH STATEMENT EXECUTE FUNCTION mizuki_reject_operator_control_audit_mutation();
`;

async function migrate(pool: Pool): Promise<void> {
  const client = await pool.connect();
  try {
    await client.query('BEGIN');
    await client.query("SELECT pg_advisory_xact_lock(hashtext('mizuki-core-schema'))");
    await client.query(`
      CREATE TABLE IF NOT EXISTS mizuki_schema_migrations (
        component text NOT NULL,
        version integer NOT NULL CHECK (version > 0),
        name text NOT NULL,
        checksum text NOT NULL CHECK (checksum ~ '^[a-f0-9]{64}$'),
        applied_at timestamptz NOT NULL DEFAULT now(),
        PRIMARY KEY (component, version)
      )
    `);
    const components = [
      {
        name: 'core',
        migrations: [{ version: 1, name: 'commercial-core', sql: COMMERCIAL_CORE_SCHEMA_V1 }],
      },
      {
        name: 'workbench',
        migrations: [{ version: 1, name: 'workbench-accounts', sql: WORKBENCH_ACCOUNTS_SCHEMA_V1 }],
      },
      {
        name: 'github-oauth',
        migrations: [{ version: 1, name: 'browser-bound-flow', sql: GITHUB_OAUTH_FLOW_SCHEMA_V1 }],
      },
      {
        name: 'admission-control',
        migrations: [
          { version: 1, name: 'admission-control-audit', sql: ADMISSION_CONTROL_AUDIT_SCHEMA },
        ],
      },
    ];
    for (const component of components) {
      const applied = await client.query<{ version: number; name: string; checksum: string }>(
        'SELECT version, name, checksum FROM mizuki_schema_migrations WHERE component = $1',
        [component.name],
      );
      const migrations = component.migrations.map((migration) => ({
        ...migration,
        checksum: createHash('sha256').update(migration.sql).digest('hex'),
      }));
      if (
        applied.rows.some(
          (row) => !migrations.some((migration) => migration.version === Number(row.version)),
        )
      ) {
        throw new Error(`${component.name} database contains an unknown schema migration`);
      }
      for (const migration of migrations) {
        const current = applied.rows.find((row) => Number(row.version) === migration.version);
        if (
          current &&
          (current.name !== migration.name || current.checksum !== migration.checksum)
        ) {
          throw new Error(`${component.name} database migration does not match this build`);
        }
        if (current) continue;
        if (applied.rows.some((row) => Number(row.version) > migration.version)) {
          throw new Error(`${component.name} database contains a schema migration gap`);
        }
        await client.query(migration.sql);
        await client.query(
          `INSERT INTO mizuki_schema_migrations (component, version, name, checksum)
           VALUES ($1, $2, $3, $4)`,
          [component.name, migration.version, migration.name, migration.checksum],
        );
      }
    }
    await client.query('COMMIT');
  } catch (error) {
    await client.query('ROLLBACK');
    throw error;
  } finally {
    client.release();
  }
}
