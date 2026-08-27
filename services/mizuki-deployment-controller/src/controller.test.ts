import { describe, expect, it } from 'vitest';
import type { ArtifactGateway } from './artifact.js';
import { DeploymentController } from './controller.js';
import {
  ControllerError,
  operationEvent,
  requestHash,
  type ShadowAdoptionRequest,
  type ShadowRequest,
} from './domain.js';
import type { ApplicationGateway } from './probe.js';
import type { RenderDeploy, RenderGateway, RenderService } from './render.js';
import { MemoryOperationStore } from './store.js';

const CANDIDATE = 'a'.repeat(40);
const MERGE = 'b'.repeat(40);
const ARTIFACT = 'f'.repeat(64);
const BASELINE_ARTIFACT = 'c'.repeat(64);
const UPGRADE = 'upgrade-1';
const IMAGE_REPOSITORY = 'ghcr.io/open-covenant/mizuki-api';

describe('deployment controller', () => {
  it('deploys the reviewed image, restores shadow, promotes, and rolls back', async () => {
    const context = createContext();

    await expect(context.controller.readiness()).resolves.toBeUndefined();
    const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    expect(started).toEqual({ deploymentId: 'dep-1' });
    await expect(context.controller.startShadow(fixture(), `${UPGRADE}:shadow`)).resolves.toEqual(
      started,
    );
    expect(context.artifacts.calls).toHaveLength(1);
    expect(context.render.mutations).toEqual([
      {
        action: 'deploy',
        serviceId: 'srv-shadow123',
        target: imageRef(ARTIFACT),
      },
    ]);

    await expect(context.controller.shadowHealth(started.deploymentId)).resolves.toMatchObject({
      status: 'starting',
      candidateSha: CANDIDATE,
      environment: 'shadow',
    });
    context.render.setLive('srv-shadow123', started.deploymentId);
    await expect(context.controller.shadowHealth(started.deploymentId)).resolves.toMatchObject({
      status: 'healthy',
    });

    const promotionInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: started.deploymentId,
      candidateSha: CANDIDATE,
      mergeSha: MERGE,
    };
    await expect(
      context.controller.promote(promotionInput, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
    expect(context.render.mutations[1]).toEqual({
      action: 'rollback',
      serviceId: 'srv-shadow123',
      target: 'dep-shadow-baseline',
    });
    context.render.setLive('srv-shadow123', 'dep-2');
    const promotion = await context.controller.promote(promotionInput, `${UPGRADE}:promote`);
    expect(promotion).toEqual({ status: 'completed', operationId: 'dep-3' });
    expect(context.render.mutations[2]).toEqual({
      action: 'deploy',
      serviceId: 'srv-production123',
      target: imageRef(ARTIFACT),
    });

    context.render.setLive('srv-production123', promotion.operationId);
    await expect(context.controller.promotionHealth(started.deploymentId)).resolves.toMatchObject({
      status: 'healthy',
      active: true,
      mergeSha: MERGE,
      promotionOperationId: promotion.operationId,
    });

    const rollbackInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: started.deploymentId,
      candidateSha: CANDIDATE,
      promotionOperationId: promotion.operationId,
      reason: 'forced canary rollback',
    };
    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).rejects.toMatchObject({ code: 'rollback_in_progress' });
    expect(context.render.mutations[3]).toEqual({
      action: 'rollback',
      serviceId: 'srv-production123',
      target: 'dep-production-baseline',
    });
    context.render.setLive('srv-production123', 'dep-4');
    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).resolves.toEqual({ status: 'completed', operationId: 'dep-4' });

    expect((await context.store.events(UPGRADE)).map((event) => event.type)).toEqual(
      expect.arrayContaining([
        'shadow_reserved',
        'artifact_verified',
        'shadow_triggering',
        'shadow_healthy',
        'shadow_restored',
        'promotion_triggering',
        'promotion_healthy',
        'rollback_triggering',
        'rollback_completed',
      ]),
    );
  });

  it('checks idempotency before downloading an artifact again', async () => {
    const context = createContext();
    const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    expect(context.artifacts.calls).toHaveLength(1);

    const changed = fixture();
    changed.artifact.url = 'https://objects.githubusercontent.com/different.json';
    await expect(
      context.controller.startShadow(changed, `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'idempotency_conflict' });
    expect(context.artifacts.calls).toHaveLength(1);
    expect(started.deploymentId).toBe('dep-1');
  });

  it('holds the production slot through final verification, then admits the next upgrade', async () => {
    let clock = new Date('2026-08-23T12:00:00.000Z');
    const context = createContext(() => clock);
    const firstShadow = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    context.render.setLive('srv-shadow123', firstShadow.deploymentId);
    await context.controller.shadowHealth(firstShadow.deploymentId);
    const firstPromotionInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: firstShadow.deploymentId,
      candidateSha: CANDIDATE,
      mergeSha: MERGE,
    };
    await expect(
      context.controller.promote(firstPromotionInput, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
    context.render.setLive('srv-shadow123', 'dep-2');
    const firstPromotion = await context.controller.promote(
      firstPromotionInput,
      `${UPGRADE}:promote`,
    );
    context.render.setLive('srv-production123', firstPromotion.operationId);
    await context.controller.promotionHealth(firstShadow.deploymentId);

    const second = fixture();
    second.upgradeId = 'upgrade-2';
    second.proposalId = 'proposal-2';
    const secondShadow = await context.controller.startShadow(second, 'upgrade-2:shadow');
    context.render.setLive('srv-shadow123', secondShadow.deploymentId);
    await context.controller.shadowHealth(secondShadow.deploymentId);
    const secondPromotion = {
      version: 1 as const,
      upgradeId: 'upgrade-2',
      proposalId: 'proposal-2',
      deploymentId: secondShadow.deploymentId,
      candidateSha: CANDIDATE,
      mergeSha: MERGE,
    };
    await expect(
      context.controller.promote(secondPromotion, 'upgrade-2:promote'),
    ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
    context.render.setLive('srv-shadow123', 'dep-5');
    await expect(
      context.controller.promote(secondPromotion, 'upgrade-2:promote'),
    ).rejects.toMatchObject({ code: 'production_busy' });

    const finalize = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: firstShadow.deploymentId,
      candidateSha: CANDIDATE,
      mergeSha: MERGE,
      promotionOperationId: firstPromotion.operationId,
    };
    await expect(
      context.controller.finalize(finalize, `${UPGRADE}:finalize`),
    ).rejects.toMatchObject({ code: 'promotion_soak_in_progress' });

    clock = new Date(clock.getTime() + 10_000);
    await expect(context.controller.finalize(finalize, `${UPGRADE}:finalize`)).resolves.toEqual({
      status: 'completed',
      operationId: firstPromotion.operationId,
    });
    await expect(context.controller.finalize(finalize, `${UPGRADE}:finalize`)).resolves.toEqual({
      status: 'completed',
      operationId: firstPromotion.operationId,
    });
    expect(
      (await context.store.events(UPGRADE)).filter(
        (event) => event.type === 'production_finalized',
      ),
    ).toHaveLength(1);

    await expect(
      context.controller.promote(secondPromotion, 'upgrade-2:promote'),
    ).resolves.toMatchObject({ status: 'completed' });
  });

  it('reconciles a lost Render receipt without repeating the mutation', async () => {
    const context = createContext();
    context.render.loseNextDeployResponse = true;

    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'render_unavailable' });
    await expect(context.controller.startShadow(fixture(), `${UPGRADE}:shadow`)).resolves.toEqual({
      deploymentId: 'dep-1',
    });
    expect(context.render.mutations).toHaveLength(1);
    expect(context.artifacts.calls).toHaveLength(1);
  });

  it('reconciles active operation evidence during readiness', async () => {
    const context = createContext();
    context.render.loseNextDeployResponse = true;
    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'render_unavailable' });

    await expect(context.controller.readiness()).resolves.toBeUndefined();
    await expect(context.store.get(UPGRADE)).resolves.toMatchObject({
      shadowState: 'triggered',
      shadowDeployId: 'dep-1',
    });
    expect(context.render.mutations).toHaveLength(1);
  });

  it('never repeats an uncertain mutation with no independently recovered receipt', async () => {
    let clock = new Date('2026-08-23T12:00:00.000Z');
    const context = createContext(() => clock);
    context.render.rejectNextDeployBeforeAcceptance = true;

    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'render_unavailable' });
    clock = new Date(clock.getTime() + 121_000);
    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'deployment_reconciliation_unresolved', status: 409 });
    expect(context.render.mutations).toHaveLength(1);
  });

  it('releases the shadow slot after permanent artifact rejection', async () => {
    const context = createContext();
    context.artifacts.failNext = new ControllerError(
      'artifact_not_oci_manifest',
      'not an image manifest',
      422,
    );
    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'artifact_not_oci_manifest' });
    expect(await context.store.activeShadow()).toBeNull();
    await expect(
      context.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'artifact_rejected', status: 409 });

    const next = fixture();
    next.upgradeId = 'upgrade-2';
    next.proposalId = 'proposal-2';
    await expect(context.controller.startShadow(next, 'upgrade-2:shadow')).resolves.toEqual({
      deploymentId: 'dep-1',
    });
  });

  it('restores the baseline after functional shadow failure', async () => {
    const context = createContext();
    const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    context.render.setLive('srv-shadow123', started.deploymentId);
    context.applications.failNext = new ControllerError(
      'application_probe_unhealthy',
      'dependency failed',
      502,
    );

    await expect(context.controller.shadowHealth(started.deploymentId)).rejects.toMatchObject({
      code: 'shadow_restore_in_progress',
    });
    expect(context.render.mutations[1]).toMatchObject({
      action: 'rollback',
      target: 'dep-shadow-baseline',
    });
    context.render.setLive('srv-shadow123', 'dep-2');
    await expect(context.controller.shadowHealth(started.deploymentId)).resolves.toMatchObject({
      status: 'unhealthy',
    });
    expect(await context.store.activeShadow()).toBeNull();
  });

  it('recovers a lost promotion ID and rolls back without caller-held evidence', async () => {
    const context = createContext();
    const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    context.render.setLive('srv-shadow123', started.deploymentId);
    await context.controller.shadowHealth(started.deploymentId);
    const promoteInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: started.deploymentId,
      candidateSha: CANDIDATE,
      mergeSha: MERGE,
    };
    await expect(
      context.controller.promote(promoteInput, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
    context.render.setLive('srv-shadow123', 'dep-2');
    context.render.loseNextDeployResponse = true;
    await expect(
      context.controller.promote(promoteInput, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'render_unavailable' });

    const rollbackInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: started.deploymentId,
      candidateSha: CANDIDATE,
      reason: 'promotion receipt lost',
    };
    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).rejects.toMatchObject({ code: 'rollback_in_progress' });
    expect(context.render.mutations[3]).toEqual({
      action: 'rollback',
      serviceId: 'srv-production123',
      target: 'dep-production-baseline',
    });
    context.render.setLive('srv-production123', 'dep-4');
    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).resolves.toEqual({ status: 'completed', operationId: 'dep-4' });
  });

  it('restores shadow when rollback is requested before promotion', async () => {
    const context = createContext();
    const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    context.render.setLive('srv-shadow123', started.deploymentId);
    await context.controller.shadowHealth(started.deploymentId);
    const rollbackInput = {
      version: 1 as const,
      upgradeId: UPGRADE,
      proposalId: 'proposal-1',
      deploymentId: started.deploymentId,
      candidateSha: CANDIDATE,
      reason: 'checks changed before merge',
    };

    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
    expect(context.render.mutations[1]).toEqual({
      action: 'rollback',
      serviceId: 'srv-shadow123',
      target: 'dep-shadow-baseline',
    });
    context.render.setLive('srv-shadow123', 'dep-2');
    await expect(
      context.controller.rollback(rollbackInput, `${UPGRADE}:rollback`),
    ).resolves.toMatchObject({ status: 'completed' });
    expect(context.render.mutations).toHaveLength(2);
  });

  it('adopts a healthy candidate after its exact baseline restore fails before deploy', async () => {
    const context = createContext();
    const recovery = await prepareFailedShadowRestore(context);
    const mutationCount = context.render.mutations.length;

    await expect(
      context.controller.adoptShadow(recovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).resolves.toEqual({ status: 'completed', operationId: recovery.started.deploymentId });
    await expect(
      context.controller.adoptShadow(recovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).resolves.toEqual({ status: 'completed', operationId: recovery.started.deploymentId });

    expect(context.render.mutations).toHaveLength(mutationCount);
    expect(await context.store.activeShadow()).toBeNull();
    await expect(context.store.get(UPGRADE)).resolves.toMatchObject({
      shadowRestoreState: 'failed',
      shadowRestoreDeployId: recovery.adoption.restoreDeploymentId,
      shadowActive: false,
      shadowBaselineDeployId: 'dep-shadow-baseline',
      shadowBaselineArtifactSha256: BASELINE_ARTIFACT,
    });
    const adoptionEvents = (await context.store.events(UPGRADE)).filter(
      (event) => event.type === 'shadow_baseline_adopted',
    );
    expect(adoptionEvents).toHaveLength(1);
    expect(adoptionEvents[0]?.detail).toMatchObject({
      idempotencyKey: `${UPGRADE}:adopt-shadow`,
      requestHash: requestHash(recovery.adoption),
      failedRestoreDeployId: recovery.adoption.restoreDeploymentId,
    });
    await expect(context.controller.readiness()).resolves.toBeUndefined();
    await expect(
      context.controller.promote(recovery.promotion, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'shadow_baseline_adopted', status: 409 });
    await expect(
      context.controller.rollback(recovery.rollback, `${UPGRADE}:rollback`),
    ).rejects.toMatchObject({ code: 'shadow_baseline_adopted', status: 409 });

    const next = fixture();
    next.upgradeId = 'upgrade-2';
    next.proposalId = 'proposal-2';
    await expect(context.controller.startShadow(next, 'upgrade-2:shadow')).resolves.toEqual({
      deploymentId: 'dep-3',
    });
    await expect(context.store.get('upgrade-2')).resolves.toMatchObject({
      shadowBaselineDeployId: recovery.started.deploymentId,
      shadowBaselineArtifactSha256: ARTIFACT,
    });
  });

  it('keeps the shadow slot locked unless the exact failed restore and live candidate are proven', async () => {
    const pending = createContext();
    const pendingRecovery = await prepareFailedShadowRestore(pending, false);
    await expect(
      pending.controller.adoptShadow(pendingRecovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).rejects.toMatchObject({ code: 'shadow_adoption_restore_mismatch', status: 409 });
    expect(await pending.store.activeShadow()).not.toBeNull();

    const drifted = createContext();
    const driftedRecovery = await prepareFailedShadowRestore(drifted);
    drifted.render.addExternal('srv-shadow123', '9'.repeat(64));
    await expect(
      drifted.controller.adoptShadow(driftedRecovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).rejects.toMatchObject({ code: 'shadow_drift', status: 409 });
    expect(await drifted.store.activeShadow()).not.toBeNull();

    const serviceDrift = createContext();
    const serviceDriftRecovery = await prepareFailedShadowRestore(serviceDrift);
    serviceDrift.render.services.get('srv-shadow123')!.serviceDetails.region = 'oregon';
    await expect(
      serviceDrift.controller.adoptShadow(
        serviceDriftRecovery.adoption,
        `${UPGRADE}:adopt-shadow`,
      ),
    ).rejects.toMatchObject({ code: 'render_service_drift', status: 409 });
    expect(await serviceDrift.store.activeShadow()).not.toBeNull();

    const promoted = createContext();
    const promotedRecovery = await prepareFailedShadowRestore(promoted);
    const operation = await promoted.store.get(UPGRADE);
    if (!operation) throw new Error('operation fixture is missing');
    operation.promotionIdempotencyKey = `${UPGRADE}:promote`;
    operation.promotionRequestHash = requestHash(promotedRecovery.promotion);
    operation.promotionState = 'reserved';
    await promoted.store.save(
      operation,
      operationEvent(operation, 'promotion_fixture', {}, operation.updatedAt),
    );
    await expect(
      promoted.controller.adoptShadow(promotedRecovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).rejects.toMatchObject({ code: 'shadow_adoption_not_allowed', status: 409 });
    expect(await promoted.store.activeShadow()).not.toBeNull();
  });

  it('does not release the shadow slot when the adoption application probe fails', async () => {
    const context = createContext();
    const recovery = await prepareFailedShadowRestore(context);
    context.applications.failNext = new ControllerError(
      'application_probe_unhealthy',
      'dependency failed',
      502,
    );

    await expect(
      context.controller.adoptShadow(recovery.adoption, `${UPGRADE}:adopt-shadow`),
    ).rejects.toMatchObject({ code: 'application_probe_unhealthy' });
    expect(await context.store.activeShadow()).not.toBeNull();
  });

  it('rejects repository, binding, and mutation-time service drift', async () => {
    const denied = createContext();
    const badRepository = fixture();
    badRepository.repository.owner = 'other';
    await expect(
      denied.controller.startShadow(badRepository, `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'repository_denied' });
    expect(denied.render.mutations).toHaveLength(0);

    const binding = createContext();
    const started = await binding.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    await expect(
      binding.controller.promote(
        {
          version: 1,
          upgradeId: UPGRADE,
          proposalId: 'proposal-1',
          deploymentId: started.deploymentId,
          candidateSha: 'd'.repeat(40),
          mergeSha: MERGE,
        },
        `${UPGRADE}:promote`,
      ),
    ).rejects.toMatchObject({ code: 'operation_binding_mismatch' });

    const drift = createContext();
    drift.artifacts.afterVerify = () => {
      drift.render.services.get('srv-shadow123')!.imagePath = 'ghcr.io/open-covenant/other:latest';
    };
    await expect(
      drift.controller.startShadow(fixture(), `${UPGRADE}:shadow`),
    ).rejects.toMatchObject({ code: 'artifact_execution_unbound' });
    expect(drift.render.mutations).toHaveLength(0);
  });

  it('accepts a legacy fingerprint only when the restored service is unchanged', async () => {
    const context = createContext();
    const baseline = structuredClone(context.render.services.get('srv-shadow123')!);
    baseline.imagePath = imageRef(BASELINE_ARTIFACT);

    await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
    const operation = await context.store.get(UPGRADE);
    if (!operation) throw new Error('operation fixture is missing');
    operation.shadowServiceFingerprint = legacyServiceFingerprint(baseline);
    await context.store.save(
      operation,
      operationEvent(operation, 'legacy_fingerprint_fixture', {}, operation.updatedAt),
    );

    await expect(context.controller.readiness()).resolves.toBeUndefined();

    context.render.services.get('srv-shadow123')!.serviceDetails.region = 'oregon';
    await expect(context.controller.readiness()).rejects.toMatchObject({
      code: 'render_service_drift',
    });
  });

  it('brackets functional probes with immutable active-deploy observations', async () => {
    const context = createContext();
    context.applications.afterProbe = (serviceId) => {
      context.render.addExternal(serviceId, '9'.repeat(64));
    };
    await expect(context.controller.readiness()).rejects.toMatchObject({
      code: 'application_probe_deploy_drift',
    });
  });

  it('rejects a live baseline that was deployed from a mutable tag', async () => {
    const context = createContext();
    context.render.setImageRef(
      'srv-shadow123',
      'dep-shadow-baseline',
      `${IMAGE_REPOSITORY}:latest`,
    );
    await expect(context.controller.readiness()).rejects.toMatchObject({
      code: 'artifact_execution_unbound',
    });
  });
});

class PassingArtifacts implements ArtifactGateway {
  readonly calls: unknown[][] = [];
  afterVerify?: () => void;
  failNext: Error | null = null;

  async verify(url: string, sha256: string, sizeBytes: number) {
    this.calls.push([url, sha256, sizeBytes]);
    if (this.failNext) {
      const error = this.failNext;
      this.failNext = null;
      throw error;
    }
    this.afterVerify?.();
    return {
      sha256,
      sizeBytes,
      mediaType: 'application/vnd.oci.image.manifest.v1+json',
    };
  }
}

class FakeApplications implements ApplicationGateway {
  readonly calls: string[] = [];
  failNext: Error | null = null;
  afterProbe?: (serviceId: string) => void;

  async probe(serviceId: string): Promise<void> {
    this.calls.push(serviceId);
    if (this.failNext) {
      const error = this.failNext;
      this.failNext = null;
      throw error;
    }
    this.afterProbe?.(serviceId);
    this.afterProbe = undefined;
  }
}

class FakeRender implements RenderGateway {
  readonly mutations: Array<{ action: string; serviceId: string; target: string }> = [];
  readonly services = new Map<string, RenderService>([
    ['srv-shadow123', service('srv-shadow123', 'private_service')],
    ['srv-production123', service('srv-production123', 'web_service')],
  ]);
  loseNextDeployResponse = false;
  rejectNextDeployBeforeAcceptance = false;
  private next = 1;
  private readonly deploys = new Map<string, RenderDeploy[]>([
    ['srv-shadow123', [deployment('dep-shadow-baseline', BASELINE_ARTIFACT, 'live', 'manual')]],
    [
      'srv-production123',
      [deployment('dep-production-baseline', BASELINE_ARTIFACT, 'live', 'manual')],
    ],
  ]);

  async service(serviceId: string): Promise<RenderService> {
    return structuredClone(this.services.get(serviceId)!);
  }

  async listDeploys(serviceId: string, createdAfter?: Date): Promise<RenderDeploy[]> {
    return structuredClone(
      (this.deploys.get(serviceId) ?? []).filter(
        (deploy) => !createdAfter || new Date(deploy.createdAt) > createdAfter,
      ),
    );
  }

  async deployImage(serviceId: string, ref: string): Promise<RenderDeploy> {
    this.mutations.push({ action: 'deploy', serviceId, target: ref });
    if (this.rejectNextDeployBeforeAcceptance) {
      this.rejectNextDeployBeforeAcceptance = false;
      throw new ControllerError('render_unavailable', 'Render API request failed', 503, true);
    }
    const deploy = this.add(serviceId, ref, 'api');
    this.services.get(serviceId)!.imagePath = ref;
    if (this.loseNextDeployResponse) {
      this.loseNextDeployResponse = false;
      throw new ControllerError('render_unavailable', 'Render API request failed', 503, true);
    }
    return structuredClone(deploy);
  }

  async rollback(serviceId: string, deployId: string): Promise<RenderDeploy> {
    const target = (this.deploys.get(serviceId) ?? []).find((deploy) => deploy.id === deployId)!;
    this.mutations.push({ action: 'rollback', serviceId, target: deployId });
    this.services.get(serviceId)!.imagePath = target.image!.ref;
    return structuredClone(this.add(serviceId, target.image!.ref, 'rollback'));
  }

  async deployment(serviceId: string, deployId: string): Promise<RenderDeploy> {
    return structuredClone(
      (this.deploys.get(serviceId) ?? []).find((deploy) => deploy.id === deployId)!,
    );
  }

  setLive(serviceId: string, deployId: string): void {
    const values = this.deploys.get(serviceId) ?? [];
    for (const deploy of values) {
      if (deploy.status === 'live') deploy.status = 'deactivated';
    }
    values.find((value) => value.id === deployId)!.status = 'live';
  }

  addExternal(serviceId: string, artifactSha256: string): void {
    const deploy = this.add(serviceId, imageRef(artifactSha256), 'manual');
    this.setLive(serviceId, deploy.id);
  }

  setImageRef(serviceId: string, deployId: string, ref: string): void {
    this.deploys.get(serviceId)!.find((deploy) => deploy.id === deployId)!.image!.ref = ref;
  }

  setStatus(serviceId: string, deployId: string, status: RenderDeploy['status']): void {
    this.deploys.get(serviceId)!.find((deploy) => deploy.id === deployId)!.status = status;
  }

  private add(serviceId: string, ref: string, trigger: RenderDeploy['trigger']): RenderDeploy {
    const artifactSha256 = ref.slice(ref.indexOf('@sha256:') + '@sha256:'.length);
    const deploy = deployment(`dep-${this.next++}`, artifactSha256, 'created', trigger, ref);
    this.deploys.get(serviceId)!.unshift(deploy);
    return deploy;
  }
}

function createContext(now: () => Date = () => new Date('2026-08-23T12:00:00.000Z')) {
  const store = new MemoryOperationStore();
  const render = new FakeRender();
  const artifacts = new PassingArtifacts();
  const applications = new FakeApplications();
  const controller = new DeploymentController(
    {
      repository: 'open-covenant/covenant',
      imageRepository: IMAGE_REPOSITORY,
      shadowServiceId: 'srv-shadow123',
      productionServiceId: 'srv-production123',
      reconciliationGraceMs: 120_000,
      minPromotionAgeMs: 10_000,
    },
    store,
    render,
    artifacts,
    applications,
    now,
  );
  return { store, render, artifacts, applications, controller };
}

function fixture(): ShadowRequest {
  return {
    version: 1,
    upgradeId: UPGRADE,
    proposalId: 'proposal-1',
    manifestSha256: 'e'.repeat(64),
    repository: {
      owner: 'open-covenant',
      name: 'covenant',
      baseBranch: 'main',
      headBranch: 'mizuki/capability/proposal-1',
    },
    candidateSha: CANDIDATE,
    artifact: {
      url: 'https://objects.githubusercontent.com/manifest.json',
      sha256: ARTIFACT,
      sizeBytes: 100,
    },
    prNumber: 42,
  };
}

async function prepareFailedShadowRestore(
  context: ReturnType<typeof createContext>,
  failRestore = true,
) {
  const started = await context.controller.startShadow(fixture(), `${UPGRADE}:shadow`);
  context.render.setLive('srv-shadow123', started.deploymentId);
  await context.controller.shadowHealth(started.deploymentId);
  const promotion = {
    version: 1 as const,
    upgradeId: UPGRADE,
    proposalId: 'proposal-1',
    deploymentId: started.deploymentId,
    candidateSha: CANDIDATE,
    mergeSha: MERGE,
  };
  await expect(
    context.controller.promote(promotion, `${UPGRADE}:promote`),
  ).rejects.toMatchObject({ code: 'shadow_restore_in_progress' });
  const restoreDeploymentId = 'dep-2';
  if (failRestore) {
    context.render.setStatus('srv-shadow123', restoreDeploymentId, 'pre_deploy_failed');
    await expect(
      context.controller.promote(promotion, `${UPGRADE}:promote`),
    ).rejects.toMatchObject({ code: 'shadow_restore_failed' });
  }
  const rollback = {
    version: 1 as const,
    upgradeId: UPGRADE,
    proposalId: 'proposal-1',
    deploymentId: started.deploymentId,
    candidateSha: CANDIDATE,
    reason: 'restore failed before production promotion',
  };
  await expect(
    context.controller.rollback(rollback, `${UPGRADE}:rollback`),
  ).rejects.toMatchObject({
    code: failRestore ? 'shadow_restore_failed' : 'shadow_restore_in_progress',
  });
  const adoption: ShadowAdoptionRequest = {
    version: 1,
    upgradeId: UPGRADE,
    proposalId: 'proposal-1',
    deploymentId: started.deploymentId,
    restoreDeploymentId,
    candidateSha: CANDIDATE,
    candidateArtifactSha256: ARTIFACT,
    baselineDeploymentId: 'dep-shadow-baseline',
    baselineArtifactSha256: BASELINE_ARTIFACT,
    reason: 'schema_incompatible_baseline',
  };
  return { started, promotion, rollback, adoption };
}

function service(id: string, type: RenderService['type']): RenderService {
  return {
    id,
    name: id,
    autoDeploy: 'no',
    imagePath: `${IMAGE_REPOSITORY}:baseline`,
    suspended: 'not_suspended',
    type,
    serviceDetails: { runtime: 'image', region: 'frankfurt', numInstances: 1 },
  };
}

function imageRef(artifactSha256: string): string {
  return `${IMAGE_REPOSITORY}@sha256:${artifactSha256}`;
}

function legacyServiceFingerprint(value: RenderService): string {
  return requestHash({
    id: value.id,
    type: value.type,
    autoDeploy: value.autoDeploy,
    imagePath: value.imagePath,
    registryCredential: value.registryCredential ?? null,
    runtime: value.serviceDetails.runtime,
    region: value.serviceDetails.region,
    numInstances: value.serviceDetails.numInstances ?? null,
    url: value.serviceDetails.url ?? null,
    suspended: value.suspended,
  });
}

function deployment(
  id: string,
  artifactSha256: string,
  status: RenderDeploy['status'],
  trigger: RenderDeploy['trigger'],
  ref = imageRef(artifactSha256),
): RenderDeploy {
  return {
    id,
    image: { ref, sha: `sha256:${artifactSha256}` },
    status,
    trigger,
    createdAt: '2026-08-23T12:00:00.000Z',
  };
}
