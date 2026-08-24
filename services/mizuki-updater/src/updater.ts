import { randomUUID } from 'node:crypto';
import type { ArtifactVerifier, ProposalVerifier } from './verification.js';
import type { DeploymentGateway } from './deployment.js';
import {
  newUpgrade,
  type AuditReceipt,
  type SignedProposal,
  type UpgradePatch,
  type UpgradeRecord,
  UpdaterError,
} from './domain.js';
import type { GitHubGateway } from './github.js';
import type { UpdaterMetrics } from './metrics.js';
import { isTerminalState, type UpgradeRepository } from './store.js';

export interface UpdaterServiceConfig {
  checkTimeoutMs: number;
  healthTimeoutMs: number;
  promotionSoakMs: number;
  promotionTimeoutMs: number;
  pollIntervalMs: number;
  leaseMs: number;
  maxAttempts: number;
}

export class UpdaterService {
  constructor(
    private readonly config: UpdaterServiceConfig,
    private readonly repository: UpgradeRepository,
    private readonly proposals: ProposalVerifier,
    private readonly artifacts: ArtifactVerifier,
    private readonly github: GitHubGateway,
    private readonly deployments: DeploymentGateway,
    private readonly metrics: UpdaterMetrics,
    private readonly now: () => Date = () => new Date(),
  ) {}

  async submit(proposal: SignedProposal, idempotencyKey: string): Promise<UpgradeRecord> {
    this.proposals.verify(proposal, this.now());
    const record = await this.repository.reserve(newUpgrade(proposal, idempotencyKey), this.now());
    this.metrics.increment('submissions');
    return record;
  }

  async get(id: string): Promise<UpgradeRecord | null> {
    return this.repository.get(id);
  }

  async getByProposalId(proposalId: string): Promise<UpgradeRecord | null> {
    return this.repository.getByProposalId(proposalId);
  }

  async audit(id: string): Promise<AuditReceipt[]> {
    return this.repository.audit(id);
  }

  kick(id: string): void {
    void this.process(id).catch(() => this.metrics.increment('errors'));
  }

  async recover(limit = 25): Promise<void> {
    const ids = await this.repository.listRunnable(this.now(), limit);
    for (const id of ids) await this.process(id);
  }

  async process(id: string): Promise<UpgradeRecord | null> {
    const owner = randomUUID();
    if (!(await this.repository.acquireLease(id, owner, this.now(), this.config.leaseMs))) {
      return this.repository.get(id);
    }
    try {
      for (let step = 0; step < 32; step += 1) {
        if (!(await this.repository.acquireLease(id, owner, this.now(), this.config.leaseMs))) {
          return this.repository.get(id);
        }
        let record = await this.repository.get(id);
        if (!record || isTerminalState(record.state)) return record;
        if (record.nextAttemptAt && record.nextAttemptAt > this.now()) return record;

        try {
          const result = await this.withLeaseHeartbeat(id, owner, () => this.step(record!, owner));
          record = result.record;
          if (!result.continue) return record;
        } catch (error) {
          const current = await this.repository.get(id);
          if (!current || isTerminalState(current.state)) return current;
          const result = await this.handleError(current, owner, normalizeError(error));
          if (!result.continue) return result.record;
        }
      }
      throw new UpdaterError('step_limit', 'Upgrade exceeded the processing step limit', 500);
    } finally {
      await this.repository.releaseLease(id, owner);
    }
  }

  private async step(
    record: UpgradeRecord,
    owner: string,
  ): Promise<{ record: UpgradeRecord; continue: boolean }> {
    const manifest = record.envelope.manifest;
    switch (record.state) {
      case 'submitted':
        return this.next(
          record,
          owner,
          { state: 'verifying_artifact' },
          'proposal_signature_verified',
          { keyId: record.envelope.keyId, manifestSha256: record.envelope.manifestSha256 },
        );

      case 'verifying_artifact': {
        const receipt = await this.artifacts.verify(
          manifest.artifact.url,
          manifest.artifact.sha256,
          manifest.artifact.sizeBytes,
        );
        this.metrics.increment('artifact_verifications');
        return this.next(record, owner, { state: 'proposal_verified' }, 'artifact_verified', {
          ...receipt,
        });
      }

      case 'proposal_verified':
        return this.next(record, owner, { state: 'syncing_pr' }, 'pull_request_sync_started');

      case 'syncing_pr': {
        const pull = await this.github.syncPullRequest(manifest, record.envelope.manifestSha256);
        this.metrics.increment('pull_requests');
        return this.next(
          record,
          owner,
          {
            state: 'waiting_checks',
            prNumber: pull.number,
            prUrl: pull.url,
            waitStartedAt: this.now(),
          },
          'pull_request_ready',
          { ...pull },
        );
      }

      case 'waiting_checks': {
        if (record.prNumber === null) throw new Error('Pull request number is missing');
        const checks = await this.github.requiredChecks(manifest, record.prNumber);
        this.metrics.increment('check_polls');
        if (checks.status === 'failed') {
          return this.fail(record, owner, 'required_check_failed', 'A required check failed', {
            checks: checks.checks,
          });
        }
        if (checks.status === 'passed') {
          return this.next(
            record,
            owner,
            { state: 'starting_shadow', waitStartedAt: null },
            'required_checks_passed',
            { checks: checks.checks },
          );
        }
        if (this.elapsed(record.waitStartedAt) >= this.config.checkTimeoutMs) {
          return this.fail(record, owner, 'required_checks_timeout', 'Required checks timed out', {
            checks: checks.checks,
          });
        }
        return this.wait(record, owner, 'required_checks_pending', { checks: checks.checks });
      }

      case 'starting_shadow': {
        if (record.prNumber === null) throw new Error('Pull request number is missing');
        const shadow = await this.deployments.startShadow(
          record.id,
          manifest,
          record.envelope.manifestSha256,
          record.prNumber,
        );
        this.metrics.increment('shadow_deployments');
        return this.next(
          record,
          owner,
          {
            state: 'checking_shadow',
            deploymentId: shadow.deploymentId,
            waitStartedAt: this.now(),
          },
          'shadow_deployment_started',
          { ...shadow },
        );
      }

      case 'checking_shadow': {
        if (!record.deploymentId) throw new Error('Deployment ID is missing');
        this.metrics.increment('shadow_health_polls');
        const health = await this.deployments.shadowHealth(
          record.deploymentId,
          manifest.candidateSha,
        );
        if (health.status === 'unhealthy') {
          return this.rollback(
            record,
            owner,
            'shadow_unhealthy',
            health.detail ?? 'Shadow is unhealthy',
          );
        }
        if (health.status === 'healthy') {
          return this.next(
            record,
            owner,
            { state: 'merging', waitStartedAt: null },
            'shadow_healthy',
            { deploymentId: record.deploymentId },
          );
        }
        if (this.elapsed(record.waitStartedAt) >= this.config.healthTimeoutMs) {
          return this.rollback(record, owner, 'shadow_health_timeout', 'Shadow health timed out');
        }
        return this.wait(record, owner, 'shadow_health_pending', {
          deploymentId: record.deploymentId,
        });
      }

      case 'merging': {
        if (record.prNumber === null) throw new Error('Pull request number is missing');
        if (!record.deploymentId) throw new Error('Deployment ID is missing');
        const reservation = await this.repository.reservePromotion(record.id, this.now());
        if (!reservation.reserved) {
          if (reservation.reason === 'disabled') {
            return this.pausePromotion(record, owner, reservation.control);
          }
          return this.wait(record, owner, 'promotion_reservation_busy', {
            activeUpgradeId: reservation.control.activeUpgradeId,
          });
        }
        return this.next(record, owner, { state: 'merge_triggering' }, 'promotion_reserved', {
          reservedAt: reservation.control.activeSince?.toISOString() ?? null,
        });
      }

      case 'merge_triggering': {
        if (record.prNumber === null) throw new Error('Pull request number is missing');
        if (!record.deploymentId) throw new Error('Deployment ID is missing');
        const existing = await this.github.mergeState(manifest, record.prNumber);
        if (existing.status === 'merged') {
          return this.next(
            record,
            owner,
            { state: 'promoting', mergeSha: existing.mergeSha },
            'pull_request_merge_reconciled',
            { mergeSha: existing.mergeSha },
          );
        }

        this.metrics.increment('shadow_health_polls');
        const health = await this.deployments.shadowHealth(
          record.deploymentId,
          manifest.candidateSha,
        );
        if (health.status !== 'healthy') {
          return this.rollback(
            record,
            owner,
            'shadow_health_changed',
            'Shadow was not healthy immediately before merge',
            { health: health.status, detail: health.detail },
          );
        }
        const checks = await this.github.requiredChecks(manifest, record.prNumber);
        this.metrics.increment('check_polls');
        if (checks.status !== 'passed') {
          return this.rollback(
            record,
            owner,
            'required_checks_changed',
            'Required checks were not passing immediately before merge',
            { checks: checks.checks },
          );
        }
        const merge = await this.github.merge(manifest, record.prNumber);
        return this.next(
          record,
          owner,
          { state: 'promoting', mergeSha: merge.mergeSha },
          'pull_request_merged',
          { ...merge },
        );
      }

      case 'promoting': {
        if (!record.deploymentId || !record.mergeSha)
          throw new Error('Promotion receipt is incomplete');
        const promotion = await this.deployments.promote(
          record.id,
          record.deploymentId,
          manifest,
          record.mergeSha,
        );
        this.metrics.increment('promotions');
        return this.next(
          record,
          owner,
          {
            state: 'verifying_promotion',
            promotionOperationId: promotion.operationId,
            promotionHealthyAt: null,
            waitStartedAt: this.now(),
          },
          'promotion_verification_started',
          {
            deploymentId: record.deploymentId,
            mergeSha: record.mergeSha,
            promotionOperationId: promotion.operationId,
            candidateSha: manifest.candidateSha,
          },
        );
      }

      case 'verifying_promotion': {
        if (
          !record.deploymentId ||
          !record.mergeSha ||
          !record.promotionOperationId ||
          !record.waitStartedAt
        )
          throw new Error('Promoted deployment receipt is incomplete');
        if (
          record.promotionHealthyAt &&
          record.promotionHealthyAt.getTime() < record.waitStartedAt.getTime()
        ) {
          throw new Error('Promotion health receipt predates verification');
        }
        if (this.elapsed(record.waitStartedAt) >= this.config.promotionTimeoutMs) {
          return this.rollback(
            record,
            owner,
            'promotion_health_timeout',
            'Promoted deployment health verification timed out',
            this.promotionEvidence(record, 'timeout'),
          );
        }

        this.metrics.increment('promotion_health_polls');
        const health = await this.deployments.promotionHealth(
          record.deploymentId,
          manifest.candidateSha,
          record.mergeSha,
          record.promotionOperationId,
        );
        const evidence = this.promotionEvidence(
          record,
          health.status,
          health.detail,
          health.active,
        );
        if (health.status === 'unhealthy') {
          return this.rollback(
            record,
            owner,
            record.promotionHealthyAt ? 'promotion_health_regressed' : 'promotion_unhealthy',
            health.detail ?? 'Promoted deployment is unhealthy',
            evidence,
          );
        }
        if (health.status === 'starting') {
          if (record.promotionHealthyAt) {
            return this.rollback(
              record,
              owner,
              'promotion_health_regressed',
              health.detail ?? 'Promoted deployment regressed after becoming healthy',
              evidence,
            );
          }
          return this.wait(record, owner, 'promotion_health_pending', evidence);
        }

        if (!record.promotionHealthyAt) {
          const observedAt = this.now();
          return this.wait(
            record,
            owner,
            'promotion_healthy_observed',
            { ...evidence, soakStartedAt: observedAt.toISOString() },
            { promotionHealthyAt: observedAt },
          );
        }
        if (this.elapsed(record.promotionHealthyAt) < this.config.promotionSoakMs) {
          return this.wait(record, owner, 'promotion_soak_healthy', evidence);
        }

        await this.deployments.finalize(
          record.id,
          record.deploymentId,
          manifest,
          record.mergeSha,
          record.promotionOperationId,
        );
        const completed = await this.next(
          record,
          owner,
          { state: 'completed', waitStartedAt: null },
          'promotion_soak_completed',
          evidence,
        );
        await this.repository.releasePromotion(record.id, this.now());
        this.metrics.increment('completions');
        return completed;
      }

      case 'rollback_pending': {
        if (!record.deploymentId) throw new Error('Rollback deployment ID is missing');
        await this.deployments.rollback(
          record.id,
          record.deploymentId,
          manifest,
          record.lastErrorCode ?? 'upgrade_failed',
          record.promotionOperationId,
        );
        this.metrics.increment('rollbacks');
        this.metrics.increment('failures');
        const rolledBack = await this.next(
          record,
          owner,
          { state: 'rolled_back' },
          'deployment_rolled_back',
          {
            deploymentId: record.deploymentId,
            reason: record.lastErrorCode,
          },
          false,
        );
        await this.repository.releasePromotion(record.id, this.now());
        return rolledBack;
      }

      case 'completed':
      case 'rolled_back':
      case 'failed':
      case 'rollback_failed':
        return { record, continue: false };
    }
  }

  private async next(
    record: UpgradeRecord,
    owner: string,
    patch: UpgradePatch,
    event: string,
    details: Record<string, unknown> = {},
    clearError = true,
  ): Promise<{ record: UpgradeRecord; continue: boolean }> {
    const updated = await this.repository.transition(
      record.id,
      record.version,
      owner,
      {
        ...patch,
        nextAttemptAt: null,
        attemptCount: 0,
        ...(clearError ? { lastErrorCode: null, lastErrorMessage: null } : {}),
      },
      { event, details },
      this.now(),
    );
    return { record: updated, continue: !isTerminalState(updated.state) };
  }

  private async wait(
    record: UpgradeRecord,
    owner: string,
    event: string,
    details: Record<string, unknown>,
    patch: UpgradePatch = {},
  ): Promise<{ record: UpgradeRecord; continue: false }> {
    const updated = await this.repository.transition(
      record.id,
      record.version,
      owner,
      {
        ...patch,
        nextAttemptAt: this.nextPollAt(record),
        attemptCount: 0,
        lastErrorCode: null,
        lastErrorMessage: null,
      },
      { event, details },
      this.now(),
    );
    return { record: updated, continue: false };
  }

  private async fail(
    record: UpgradeRecord,
    owner: string,
    code: string,
    message: string,
    details: Record<string, unknown> = {},
  ): Promise<{ record: UpgradeRecord; continue: false }> {
    this.metrics.increment('failures');
    const updated = await this.repository.transition(
      record.id,
      record.version,
      owner,
      {
        state: 'failed',
        nextAttemptAt: null,
        lastErrorCode: code,
        lastErrorMessage: message,
      },
      { event: 'upgrade_failed', details: { code, message, ...details } },
      this.now(),
    );
    return { record: updated, continue: false };
  }

  private async rollback(
    record: UpgradeRecord,
    owner: string,
    code: string,
    message: string,
    details: Record<string, unknown> = {},
  ): Promise<{ record: UpgradeRecord; continue: true }> {
    const updated = await this.repository.transition(
      record.id,
      record.version,
      owner,
      {
        state: 'rollback_pending',
        nextAttemptAt: null,
        attemptCount: 0,
        lastErrorCode: code,
        lastErrorMessage: message,
      },
      { event: 'rollback_required', details: { code, message, ...details } },
      this.now(),
    );
    return { record: updated, continue: true };
  }

  private async handleError(
    record: UpgradeRecord,
    owner: string,
    error: UpdaterError,
  ): Promise<{ record: UpgradeRecord; continue: boolean }> {
    if (
      record.state === 'verifying_promotion' &&
      this.elapsed(record.waitStartedAt) >= this.config.promotionTimeoutMs
    ) {
      return this.rollback(
        record,
        owner,
        'promotion_health_timeout',
        'Promoted deployment health verification timed out',
        { causeCode: error.code, causeMessage: error.message },
      );
    }
    if (error.retryable && record.attemptCount + 1 < this.config.maxAttempts) {
      this.metrics.increment('retries');
      const attemptCount = record.attemptCount + 1;
      const delay = Math.min(this.config.pollIntervalMs * 2 ** (attemptCount - 1), 60_000);
      const updated = await this.repository.transition(
        record.id,
        record.version,
        owner,
        {
          attemptCount,
          nextAttemptAt: this.retryAt(record, delay),
          lastErrorCode: error.code,
          lastErrorMessage: error.message,
        },
        { event: 'action_retry_scheduled', details: { code: error.code, attemptCount, delay } },
        this.now(),
      );
      return { record: updated, continue: false };
    }

    if (record.state === 'rollback_pending') {
      this.metrics.increment('failures');
      await this.repository.closePromotionsForFailure(
        record.id,
        `rollback failed: ${error.code}`.slice(0, 500),
        this.now(),
      );
      const updated = await this.repository.transition(
        record.id,
        record.version,
        owner,
        {
          state: 'rollback_failed',
          nextAttemptAt: null,
          lastErrorCode: error.code,
          lastErrorMessage: error.message,
        },
        { event: 'rollback_failed', details: { code: error.code, message: error.message } },
        this.now(),
      );
      return { record: updated, continue: false };
    }
    if (record.deploymentId) {
      return this.rollback(record, owner, error.code, error.message);
    }
    return this.fail(record, owner, error.code, error.message);
  }

  private async pausePromotion(
    record: UpgradeRecord,
    owner: string,
    control: { revision: number; reason: string },
  ): Promise<{ record: UpgradeRecord; continue: false }> {
    if (record.lastErrorCode === 'promotion_paused') return { record, continue: false };
    const paused = await this.repository.transition(
      record.id,
      record.version,
      owner,
      {
        nextAttemptAt: null,
        lastErrorCode: 'promotion_paused',
        lastErrorMessage: 'Promotion is closed by operator control',
      },
      {
        event: 'promotion_paused',
        details: { controlRevision: control.revision, reason: control.reason },
      },
      this.now(),
    );
    return { record: paused, continue: false };
  }

  private async withLeaseHeartbeat<T>(
    id: string,
    owner: string,
    action: () => Promise<T>,
  ): Promise<T> {
    const intervalMs = Math.max(1_000, Math.min(30_000, Math.floor(this.config.leaseMs / 3)));
    let renewal = Promise.resolve();
    let lost = false;
    const heartbeat = setInterval(() => {
      renewal = renewal.then(async () => {
        try {
          if (!(await this.repository.acquireLease(id, owner, this.now(), this.config.leaseMs))) {
            lost = true;
          }
        } catch {
          lost = true;
        }
      });
    }, intervalMs);
    heartbeat.unref();
    try {
      const result = await action();
      await renewal;
      if (lost) throw new UpdaterError('lease_lost', 'Upgrade lease was lost', 409, true);
      return result;
    } finally {
      clearInterval(heartbeat);
      await renewal;
    }
  }

  private elapsed(startedAt: Date | null): number {
    return startedAt ? this.now().getTime() - startedAt.getTime() : 0;
  }

  private nextPollAt(record: UpgradeRecord): Date {
    return this.retryAt(record, this.config.pollIntervalMs);
  }

  private retryAt(record: UpgradeRecord, delayMs: number): Date {
    const requested = this.now().getTime() + delayMs;
    if (record.state !== 'verifying_promotion' || !record.waitStartedAt) {
      return new Date(requested);
    }
    const deadline = record.waitStartedAt.getTime() + this.config.promotionTimeoutMs;
    return new Date(Math.min(requested, deadline));
  }

  private promotionEvidence(
    record: UpgradeRecord,
    health: 'starting' | 'healthy' | 'unhealthy' | 'timeout',
    detail?: string,
    active?: boolean,
  ): Record<string, unknown> {
    return {
      deploymentId: record.deploymentId,
      mergeSha: record.mergeSha,
      promotionOperationId: record.promotionOperationId,
      candidateSha: record.envelope.manifest.candidateSha,
      environment: 'production',
      active: active ?? false,
      health,
      detail,
      verificationElapsedMs: this.elapsed(record.waitStartedAt),
      healthyElapsedMs: record.promotionHealthyAt ? this.elapsed(record.promotionHealthyAt) : null,
    };
  }
}

function normalizeError(error: unknown): UpdaterError {
  if (error instanceof UpdaterError) return error;
  return new UpdaterError(
    'invalid_external_receipt',
    error instanceof Error ? error.message.slice(0, 500) : 'Unexpected upgrade error',
    502,
  );
}
