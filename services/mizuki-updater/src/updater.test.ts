import { describe, expect, it } from 'vitest';
import type {
  DeploymentGateway,
  DeploymentHealth,
  PromotionHealth,
  PromotionReceipt,
  ShadowReceipt,
} from './deployment.js';
import type { UpgradeManifest } from './domain.js';
import { UpdaterError } from './domain.js';
import type { CheckReceipt, GitHubGateway, MergeReceipt, PullRequestReceipt } from './github.js';
import { UpdaterMetrics } from './metrics.js';
import { InMemoryUpgradeRepository } from './store.js';
import { proposalFixture } from './test-utils.js';
import { UpdaterService } from './updater.js';
import type { ArtifactVerification, ArtifactVerifier } from './verification.js';
import { ProposalVerifier } from './verification.js';

const START = new Date('2026-08-22T12:00:00.000Z');

describe('autonomous upgrade state machine', () => {
  it('completes only after the promoted commit remains healthy for the soak window', async () => {
    const context = await fixture();
    context.github.checks.push(pendingChecks(), passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'starting' },
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');

    expect((await context.service.process(record.id))?.state).toBe('waiting_checks');
    context.advance(1_000);
    expect((await context.service.process(record.id))?.state).toBe('checking_shadow');
    context.advance(1_000);
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(2_000);
    expect((await context.service.process(record.id))?.state).toBe('completed');

    const completed = await context.repository.get(record.id);
    expect(completed).toMatchObject({
      prNumber: 42,
      deploymentId: 'shadow-1',
      mergeSha: 'b'.repeat(40),
      promotionOperationId: 'promotion-1',
      promotionHealthyAt: new Date('2026-08-22T12:00:02.000Z'),
    });
    expect(context.github.mergeCalls).toBe(1);
    expect(context.deployments.promoteCalls).toBe(1);
    expect(context.deployments.rollbackCalls).toBe(0);

    await context.service.process(record.id);
    expect(context.github.mergeCalls).toBe(1);
    expect(context.deployments.promoteCalls).toBe(1);
    const audit = await context.service.audit(record.id);
    expect(audit.at(-1)?.event).toBe('promotion_soak_completed');
    expect(audit.at(-1)?.details).toMatchObject({
      environment: 'production',
      active: true,
      candidateSha: context.proposal.manifest.candidateSha,
      mergeSha: 'b'.repeat(40),
      promotionOperationId: 'promotion-1',
    });
    expect(
      audit.every(
        (receipt, index) => index === 0 || receipt.previousHash === audit[index - 1].hash,
      ),
    ).toBe(true);
  });

  it('never deploys or merges when a required check fails', async () => {
    const context = await fixture();
    context.github.checks.push(failedChecks());
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('failed');
    expect(context.deployments.startCalls).toBe(0);
    expect(context.github.mergeCalls).toBe(0);
  });

  it('times out missing checks without starting a shadow deployment', async () => {
    const context = await fixture({ checkTimeoutMs: 2_000 });
    context.github.checks.push(pendingChecks(), pendingChecks());
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('waiting_checks');
    context.advance(2_001);
    expect((await context.service.process(record.id))?.state).toBe('failed');
    expect(context.deployments.startCalls).toBe(0);
  });

  it('rolls back an unhealthy shadow without merging', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks());
    context.deployments.healthResults.push({ status: 'unhealthy', detail: 'error rate high' });
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect(await context.service.process(record.id)).toMatchObject({
      state: 'rolled_back',
      lastErrorCode: 'shadow_unhealthy',
    });
    expect(context.github.mergeCalls).toBe(0);
    expect(context.deployments.rollbackCalls).toBe(1);
    expect(context.deployments.rollbackOperationIds).toEqual([null]);
  });

  it('rechecks required checks immediately before merge and rolls back if they changed', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), failedChecks());
    context.deployments.healthResults.push({ status: 'healthy' });
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('rolled_back');
    expect(context.github.mergeCalls).toBe(0);
    expect(context.deployments.rollbackCalls).toBe(1);
  });

  it('rechecks shadow health immediately before merge and rolls back if it regressed', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push({ status: 'healthy' }, { status: 'unhealthy' });
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('rolled_back');
    expect(context.github.mergeCalls).toBe(0);
    expect(context.deployments.rollbackCalls).toBe(1);
  });

  it('rolls back when promotion fails after merge', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push({ status: 'healthy' });
    context.deployments.promoteError = new UpdaterError(
      'promotion_rejected',
      'Promotion was rejected',
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('rolled_back');
    expect(context.github.mergeCalls).toBe(1);
    expect(context.deployments.rollbackCalls).toBe(1);
  });

  it('holds a merged candidate at the promotion boundary while control is closed', async () => {
    const context = await fixture({ promotionsEnabled: false });
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push({ status: 'healthy' }, { status: 'healthy' });
    const record = await context.service.submit(context.proposal, 'idempotency-1');

    expect(await context.service.process(record.id)).toMatchObject({
      state: 'promoting',
      lastErrorCode: 'promotion_paused',
    });
    expect(context.github.mergeCalls).toBe(1);
    expect(context.deployments.promoteCalls).toBe(0);
    expect(context.deployments.rollbackCalls).toBe(0);

    await context.repository.updatePromotionControl(
      {
        promotionsEnabled: true,
        expectedRevision: 0,
        reason: 'operator approved controlled promotion',
        updatedBy: 'write_authority',
      },
      START,
    );
    context.deployments.healthResults.push({ status: 'healthy' });
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    expect(context.deployments.promoteCalls).toBe(1);
  });

  it('keeps monitoring and rolls back an already promoted candidate after a pause', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');

    await context.repository.updatePromotionControl(
      {
        promotionsEnabled: false,
        expectedRevision: 1,
        reason: 'pause new promotions during incident response',
        updatedBy: 'write_authority',
      },
      START,
    );
    context.deployments.healthResults.push({ status: 'unhealthy', detail: 'error rate high' });
    context.advance(1_000);

    expect(await context.service.process(record.id)).toMatchObject({
      state: 'rolled_back',
      lastErrorCode: 'promotion_health_regressed',
    });
    expect(context.deployments.rollbackCalls).toBe(1);
    expect(context.deployments.rollbackOperationIds).toEqual(['promotion-1']);
  });

  it('rolls back when promoted health regresses during the soak', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'starting', detail: 'production instances restarting' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');

    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(1_000);
    expect(await context.service.process(record.id)).toMatchObject({
      state: 'rolled_back',
      lastErrorCode: 'promotion_health_regressed',
    });
    expect(context.deployments.rollbackCalls).toBe(1);
    expect(context.deployments.rollbackOperationIds).toEqual(['promotion-1']);
  });

  it('rolls back when promoted health never becomes ready before its deadline', async () => {
    const context = await fixture({ promotionTimeoutMs: 3_000 });
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'starting' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');

    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(3_000);
    expect(await context.service.process(record.id)).toMatchObject({
      state: 'rolled_back',
      lastErrorCode: 'promotion_health_timeout',
    });
    expect(context.deployments.healthCalls).toBe(3);
    expect(context.deployments.rollbackCalls).toBe(1);
  });

  it('resumes a durable promotion soak without replaying promotion', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');

    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(2_000);
    expect((await context.restart().process(record.id))?.state).toBe('completed');
    expect(context.deployments.promoteCalls).toBe(1);
    expect(context.deployments.promotionHealthOperationIds).toEqual(['promotion-1', 'promotion-1']);
  });

  it('rolls back after promoted health evidence exhausts its retries', async () => {
    const context = await fixture();
    context.github.checks.push(passedChecks(), passedChecks());
    context.deployments.healthResults.push(
      { status: 'healthy' },
      { status: 'healthy' },
      { status: 'healthy' },
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');

    const unavailable = () =>
      new UpdaterError('deployment_unavailable', 'Health evidence unavailable', 503, true);
    context.deployments.healthResults.push(unavailable(), unavailable(), unavailable());
    context.advance(1_000);
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(1_000);
    expect((await context.service.process(record.id))?.state).toBe('verifying_promotion');
    context.advance(2_000);
    expect(await context.service.process(record.id)).toMatchObject({
      state: 'rolled_back',
      lastErrorCode: 'deployment_unavailable',
    });
    expect(context.deployments.rollbackCalls).toBe(1);
  });

  it('durably schedules transient errors and resumes after the retry deadline', async () => {
    const context = await fixture();
    context.artifacts.errors.push(
      new UpdaterError('artifact_unavailable', 'temporary outage', 503, true),
    );
    context.github.checks.push(failedChecks());
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    const waiting = await context.service.process(record.id);
    expect(waiting).toMatchObject({
      state: 'verifying_artifact',
      attemptCount: 1,
      lastErrorCode: 'artifact_unavailable',
    });
    await context.service.process(record.id);
    expect(context.artifacts.calls).toBe(1);

    context.advance(1_000);
    expect((await context.service.process(record.id))?.state).toBe('failed');
    expect(context.artifacts.calls).toBe(2);
  });

  it('fails closed when the verified artifact does not match', async () => {
    const context = await fixture();
    context.artifacts.errors.push(
      new UpdaterError('artifact_hash_mismatch', 'Artifact hash does not match manifest'),
    );
    const record = await context.service.submit(context.proposal, 'idempotency-1');
    const result = await context.service.process(record.id);
    expect(result).toMatchObject({ state: 'failed', lastErrorCode: 'artifact_hash_mismatch' });
    expect(context.github.syncCalls).toBe(0);
  });
});

async function fixture(
  options: {
    checkTimeoutMs?: number;
    promotionTimeoutMs?: number;
    promotionsEnabled?: boolean;
  } = {},
) {
  const signed = proposalFixture(START);
  let now = new Date(START);
  const repository = new InMemoryUpgradeRepository();
  const artifacts = new FakeArtifacts(signed.proposal.manifest.artifact);
  const github = new FakeGitHub();
  const deployments = new FakeDeployments();
  const proposals = new ProposalVerifier({
    trustedProposalKeys: { 'release-key-1': signed.publicKey },
    trustedBenchmarkKeys: { 'benchmark-key-1': signed.benchmarkPublicKey },
    trustedReviewKeys: { 'review-key-1': signed.reviewPublicKey },
    allowedRepositories: new Set(['mizuki-labs/mizuki']),
    allowedBaseBranches: new Set(['main']),
    headBranchPrefix: 'mizuki/',
    mandatoryChecks: new Set(['test', 'security']),
    maxProposalAgeMs: 7 * 24 * 60 * 60_000,
  });
  const metrics = new UpdaterMetrics();
  const createService = () =>
    new UpdaterService(
      {
        checkTimeoutMs: options.checkTimeoutMs ?? 10_000,
        healthTimeoutMs: 10_000,
        promotionSoakMs: 2_000,
        promotionTimeoutMs: options.promotionTimeoutMs ?? 10_000,
        pollIntervalMs: 1_000,
        leaseMs: 30_000,
        maxAttempts: 3,
      },
      repository,
      proposals,
      artifacts,
      github,
      deployments,
      metrics,
      () => new Date(now),
    );
  const service = createService();
  if (options.promotionsEnabled !== false) {
    await repository.updatePromotionControl(
      {
        promotionsEnabled: true,
        expectedRevision: 0,
        reason: 'test promotion authorization',
        updatedBy: 'test_authority',
      },
      now,
    );
  }
  return {
    service,
    repository,
    artifacts,
    github,
    deployments,
    proposal: signed.proposal,
    restart: createService,
    advance(milliseconds: number) {
      now = new Date(now.getTime() + milliseconds);
    },
  };
}

class FakeArtifacts implements ArtifactVerifier {
  calls = 0;
  errors: Error[] = [];

  constructor(private readonly artifact: UpgradeManifest['artifact']) {}

  async verify(): Promise<ArtifactVerification> {
    this.calls += 1;
    const error = this.errors.shift();
    if (error) throw error;
    return { sha256: this.artifact.sha256, sizeBytes: this.artifact.sizeBytes };
  }
}

class FakeGitHub implements GitHubGateway {
  checks: CheckReceipt[] = [];
  syncCalls = 0;
  mergeCalls = 0;

  async syncPullRequest(): Promise<PullRequestReceipt> {
    this.syncCalls += 1;
    return { number: 42, url: 'https://github.com/mizuki-labs/mizuki/pull/42' };
  }

  async requiredChecks(): Promise<CheckReceipt> {
    return this.checks.shift() ?? pendingChecks();
  }

  async merge(): Promise<MergeReceipt> {
    this.mergeCalls += 1;
    return { mergeSha: 'b'.repeat(40) };
  }
}

class FakeDeployments implements DeploymentGateway {
  healthResults: Array<DeploymentHealth | Error> = [];
  promoteError: Error | null = null;
  startCalls = 0;
  promoteCalls = 0;
  rollbackCalls = 0;
  healthCalls = 0;
  promotionHealthOperationIds: string[] = [];
  rollbackOperationIds: Array<string | null> = [];

  async startShadow(): Promise<ShadowReceipt> {
    this.startCalls += 1;
    return { deploymentId: 'shadow-1' };
  }

  async shadowHealth(): Promise<DeploymentHealth> {
    this.healthCalls += 1;
    const result = this.healthResults.shift() ?? { status: 'healthy' as const };
    if (result instanceof Error) throw result;
    return result;
  }

  async promotionHealth(
    _deploymentId: string,
    _candidateSha: string,
    mergeSha: string,
    operationId: string,
  ): Promise<PromotionHealth> {
    this.healthCalls += 1;
    this.promotionHealthOperationIds.push(operationId);
    const result = this.healthResults.shift() ?? { status: 'healthy' as const };
    if (result instanceof Error) throw result;
    return {
      ...result,
      active: result.status === 'healthy',
      mergeSha,
      operationId,
    };
  }

  async promote(): Promise<PromotionReceipt> {
    this.promoteCalls += 1;
    if (this.promoteError) throw this.promoteError;
    return { operationId: 'promotion-1' };
  }

  async rollback(
    _upgradeId: string,
    _deploymentId: string,
    _manifest: UpgradeManifest,
    _reason: string,
    _promotionOperationId: string | null,
  ): Promise<void> {
    this.rollbackCalls += 1;
    this.rollbackOperationIds.push(_promotionOperationId);
  }
}

function passedChecks(): CheckReceipt {
  return { status: 'passed', checks: { test: 'success', security: 'success' } };
}

function pendingChecks(): CheckReceipt {
  return { status: 'pending', checks: { test: 'pending', security: 'missing' } };
}

function failedChecks(): CheckReceipt {
  return { status: 'failed', checks: { test: 'failure', security: 'success' } };
}
