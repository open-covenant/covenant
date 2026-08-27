import type { ArtifactGateway } from './artifact.js';
import type { ControllerConfig } from './config.js';
import {
  ControllerError,
  operationEvent,
  requestHash,
  type DeploymentOperation,
  type FinalizeRequest,
  type PromotionRequest,
  type RollbackRequest,
  type ShadowAdoptionRequest,
  type ShadowRequest,
} from './domain.js';
import type { ApplicationGateway } from './probe.js';
import {
  deployArtifactSha256,
  deploymentHealth,
  exactImageRef,
  serviceImageRepository,
  type RenderDeploy,
  type RenderGateway,
  type RenderService,
} from './render.js';
import type { OperationStore } from './store.js';

type Target = 'shadow' | 'production';
type ReconcileAction = 'shadow' | 'promotion' | 'shadow_restore' | 'rollback';

export class DeploymentController {
  constructor(
    private readonly config: Pick<
      ControllerConfig,
      | 'repository'
      | 'imageRepository'
      | 'shadowServiceId'
      | 'productionServiceId'
      | 'reconciliationGraceMs'
      | 'minPromotionAgeMs'
    >,
    private readonly store: OperationStore,
    private readonly render: RenderGateway,
    private readonly artifacts: ArtifactGateway,
    private readonly applications: ApplicationGateway,
    private readonly now: () => Date = () => new Date(),
  ) {}

  async readiness(): Promise<void> {
    await this.store.health();
    const [shadowService, productionService] = await Promise.all([
      this.render.service(this.config.shadowServiceId),
      this.render.service(this.config.productionServiceId),
    ]);
    this.assertService(shadowService, 'shadow');
    this.assertService(productionService, 'production');

    await this.store.withMutationLock(async () => {
      const shadow = await this.store.activeShadow();
      const production = await this.store.activeProduction();
      if (shadow) await this.reconcileForReadiness(shadow, 'shadow');
      else await this.assertHealthyLive(this.config.shadowServiceId);
      if (production) await this.reconcileForReadiness(production, 'production');
      else await this.assertHealthyLive(this.config.productionServiceId);
    });
  }

  async startShadow(
    input: ShadowRequest,
    idempotencyKey: string,
  ): Promise<{ deploymentId: string }> {
    this.assertKey(idempotencyKey, input.upgradeId, 'shadow');
    this.assertRepository(`${input.repository.owner}/${input.repository.name}`);
    const hash = requestHash(input);

    return this.store.withMutationLock(async () => {
      let operation = await this.findExisting(input.upgradeId, input.proposalId, idempotencyKey);
      if (operation) {
        this.assertShadowReplay(operation, input, idempotencyKey, hash);
        if (operation.shadowState === 'failed') {
          throw new ControllerError(
            'artifact_rejected',
            'The reviewed artifact was permanently rejected',
            409,
          );
        }
        if (operation.shadowDeployId) return { deploymentId: operation.shadowDeployId };
      } else {
        if (await this.store.activeShadow()) {
          throw new ControllerError(
            'shadow_busy',
            'The dedicated shadow service is already reserved',
            503,
            true,
            5,
          );
        }
        const service = await this.requireService('shadow');
        const baseline = await this.assertHealthyLive(this.config.shadowServiceId);
        operation = newOperation(
          input,
          idempotencyKey,
          hash,
          this.config.imageRepository,
          serviceFingerprint(service),
          baseline,
          this.now(),
        );
        await this.store.insert(
          operation,
          operationEvent(operation, 'shadow_reserved', {}, this.now()),
        );
      }

      if (!operation.artifactVerifiedAt) {
        try {
          await this.artifacts.verify(
            operation.artifactUrl,
            operation.artifactSha256,
            operation.artifactSizeBytes,
          );
        } catch (error) {
          if (error instanceof ControllerError && !error.retryable) {
            operation.shadowState = 'failed';
            operation.shadowActive = false;
            operation.updatedAt = this.now();
            await this.store.save(
              operation,
              operationEvent(operation, 'artifact_rejected', { code: error.code }, this.now()),
            );
          }
          throw error;
        }
        operation.artifactVerifiedAt = this.now();
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(
            operation,
            'artifact_verified',
            { artifactSha256: operation.artifactSha256 },
            this.now(),
          ),
        );
      }

      const deploy = await this.ensureImageDeployment(operation, 'shadow');
      operation.shadowDeployId = deploy.id;
      operation.shadowState = 'triggered';
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(operation, 'shadow_triggered', { deployId: deploy.id }, this.now()),
      );
      return { deploymentId: deploy.id };
    });
  }

  async shadowHealth(deploymentId: string): Promise<{
    status: 'starting' | 'healthy' | 'unhealthy';
    candidateSha: string;
    environment: 'shadow';
    detail?: string;
  }> {
    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(deploymentId);
      return this.evaluateShadowHealth(operation, true);
    });
  }

  async adoptShadow(
    input: ShadowAdoptionRequest,
    idempotencyKey: string,
  ): Promise<{ status: 'completed'; operationId: string }> {
    this.assertKey(idempotencyKey, input.upgradeId, 'adopt-shadow');
    const hash = requestHash(input);

    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(input.deploymentId);
      this.assertShadowAdoptionBinding(operation, input);

      const adoption = (await this.store.events(operation.upgradeId)).find(
        (event) => event.type === 'shadow_baseline_adopted',
      );
      if (adoption) {
        const expectedKey = adoption.detail.idempotencyKey;
        const expectedHash = adoption.detail.requestHash;
        if (typeof expectedKey !== 'string' || typeof expectedHash !== 'string') {
          throw internalState('Shadow adoption audit evidence is incomplete');
        }
        this.assertActionReplay(expectedKey, expectedHash, idempotencyKey, hash);
        if (operation.shadowRestoreState !== 'failed' || operation.shadowActive) {
          throw internalState('Shadow adoption evidence is incomplete');
        }
        return { status: 'completed', operationId: input.deploymentId };
      }

      this.assertShadowAdoptionState(operation);
      const service = await this.requireService('shadow');
      if (
        !matchesServiceFingerprint(
          service,
          operation.shadowServiceFingerprint,
          exactImageRef(
            `${this.config.imageRepository}@sha256:${operation.shadowBaselineArtifactSha256}`,
          ),
        )
      ) {
        throw new ControllerError(
          'render_service_drift',
          'Render service configuration changed',
          409,
        );
      }

      const restore = await this.render.deployment(
        this.config.shadowServiceId,
        input.restoreDeploymentId,
      );
      this.assertArtifact(
        restore,
        operation.shadowBaselineArtifactSha256,
        exactImageRef(
          `${this.config.imageRepository}@sha256:${operation.shadowBaselineArtifactSha256}`,
        ),
      );
      if (restore.trigger !== 'rollback' || restore.status !== 'pre_deploy_failed') {
        throw new ControllerError(
          'shadow_adoption_restore_mismatch',
          'Only the recorded failed baseline rollback can be adopted',
          409,
        );
      }

      const current = await this.currentLive(this.config.shadowServiceId);
      if (current.id !== input.deploymentId || !this.matchesArtifact(current, operation)) {
        throw new ControllerError(
          'shadow_drift',
          'The reviewed shadow candidate is not the active deployment',
          409,
        );
      }
      await this.assertBoundApplication(this.config.shadowServiceId, current);

      operation.shadowRestoreState = 'failed';
      operation.shadowActive = false;
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(
          operation,
          'shadow_baseline_adopted',
          {
            deployId: input.deploymentId,
            failedRestoreDeployId: input.restoreDeploymentId,
            artifactSha256: operation.artifactSha256,
            idempotencyKey,
            requestHash: hash,
            reason: input.reason,
          },
          this.now(),
        ),
      );
      return { status: 'completed', operationId: input.deploymentId };
    });
  }

  async promote(
    input: PromotionRequest,
    idempotencyKey: string,
  ): Promise<{ status: 'completed'; operationId: string }> {
    this.assertKey(idempotencyKey, input.upgradeId, 'promote');
    const hash = requestHash(input);
    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(input.deploymentId);
      this.assertBinding(operation, input);
      this.assertShadowNotAdopted(operation);
      if (operation.promotionIdempotencyKey) {
        this.assertActionReplay(
          operation.promotionIdempotencyKey,
          operation.promotionRequestHash,
          idempotencyKey,
          hash,
        );
      } else {
        if (operation.shadowRestoreState) {
          if (operation.shadowState !== 'completed') {
            throw new ControllerError('shadow_not_healthy', 'Shadow was not healthy', 422);
          }
          await this.restoreShadow(operation, 'promotion');
        } else {
          const shadow = await this.evaluateShadowHealth(operation, false);
          if (shadow.status !== 'healthy') {
            throw new ControllerError('shadow_not_healthy', 'Shadow is not healthy', 422);
          }
          await this.restoreShadow(operation, 'promotion');
        }
        await this.releasePriorProduction(operation.upgradeId);
        const service = await this.requireService('production');
        const baseline = await this.assertHealthyLive(this.config.productionServiceId);
        operation.promotionIdempotencyKey = idempotencyKey;
        operation.promotionRequestHash = hash;
        operation.promotionState = 'reserved';
        operation.mergeSha = input.mergeSha;
        operation.productionServiceFingerprint = serviceFingerprint(service);
        operation.productionBaselineDeployId = baseline.id;
        operation.productionBaselineArtifactSha256 = deployArtifactSha256(baseline);
        operation.shadowActive = false;
        operation.productionActive = true;
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, 'promotion_reserved', {}, this.now()),
        );
      }

      if (!operation.mergeSha) throw internalState('Promotion merge SHA is missing');
      if (!operation.promotionDeployId) {
        const deploy = await this.ensureImageDeployment(operation, 'promotion');
        operation.promotionDeployId = deploy.id;
        operation.promotionState = 'triggered';
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, 'promotion_triggered', { deployId: deploy.id }, this.now()),
        );
      }
      return { status: 'completed', operationId: operation.promotionDeployId };
    });
  }

  async promotionHealth(deploymentId: string): Promise<{
    status: 'starting' | 'healthy' | 'unhealthy';
    candidateSha: string;
    environment: 'production';
    active: boolean;
    mergeSha: string;
    promotionOperationId: string;
    detail?: string;
  }> {
    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(deploymentId);
      if (!operation.promotionDeployId || !operation.mergeSha) {
        throw new ControllerError('promotion_not_found', 'Promotion was not found', 404);
      }
      const deploy = await this.render.deployment(
        this.config.productionServiceId,
        operation.promotionDeployId,
      );
      if (!this.matchesArtifact(deploy, operation)) {
        return promotionEvidence(
          operation,
          'unhealthy',
          false,
          'Promotion artifact does not match',
        );
      }
      const health = deploymentHealth(deploy);
      if (health !== 'healthy') {
        return promotionEvidence(
          operation,
          health,
          false,
          health === 'unhealthy' ? `Render deploy status is ${deploy.status}` : undefined,
        );
      }
      const current = await this.currentLive(this.config.productionServiceId);
      if (current.id !== deploy.id) {
        if (
          current.id === operation.productionBaselineDeployId &&
          this.withinReconciliationGrace(deploy)
        ) {
          return promotionEvidence(operation, 'starting', false);
        }
        return promotionEvidence(
          operation,
          'unhealthy',
          false,
          'Another production deploy is active',
        );
      }
      try {
        await this.assertBoundApplication(this.config.productionServiceId, deploy);
      } catch (error) {
        if (error instanceof ControllerError && error.retryable) throw error;
        return promotionEvidence(operation, 'unhealthy', false, 'Application checks failed');
      }
      if (operation.promotionState !== 'completed') {
        operation.promotionState = 'completed';
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, 'promotion_healthy', {}, this.now()),
        );
      }
      return promotionEvidence(operation, 'healthy', true);
    });
  }

  async finalize(
    input: FinalizeRequest,
    idempotencyKey: string,
  ): Promise<{ status: 'completed'; operationId: string }> {
    this.assertKey(idempotencyKey, input.upgradeId, 'finalize');
    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(input.deploymentId);
      this.assertBinding(operation, input);
      this.assertShadowNotAdopted(operation);
      if (
        operation.mergeSha !== input.mergeSha ||
        operation.promotionDeployId !== input.promotionOperationId
      ) {
        throw new ControllerError(
          'operation_binding_mismatch',
          'Finalization does not match the recorded promotion',
          409,
        );
      }
      if (operation.productionFinalizedAt) {
        return { status: 'completed', operationId: input.promotionOperationId };
      }
      if (!operation.productionActive || operation.promotionState !== 'completed') {
        throw new ControllerError(
          'promotion_not_ready',
          'Promotion has not reached healthy active production',
          409,
        );
      }
      if (!operation.promotionStartedAt) throw internalState('Promotion start time is missing');
      const remaining =
        this.config.minPromotionAgeMs -
        (this.now().getTime() - operation.promotionStartedAt.getTime());
      if (remaining > 0) {
        throw new ControllerError(
          'promotion_soak_in_progress',
          'Promotion has not reached the minimum finalization age',
          503,
          true,
          Math.max(1, Math.ceil(remaining / 1_000)),
        );
      }

      const deploy = await this.assertPromotionEvidence(operation);
      if (deploymentHealth(deploy) !== 'healthy') {
        throw new ControllerError('promotion_unhealthy', 'Promotion is not healthy', 409);
      }
      const current = await this.currentLive(this.config.productionServiceId);
      if (current.id !== deploy.id || !this.matchesArtifact(current, operation)) {
        throw new ControllerError(
          'production_drift',
          'Promotion is not the active production deployment',
          409,
        );
      }
      await this.assertBoundApplication(this.config.productionServiceId, deploy);
      operation.productionFinalizedAt = this.now();
      operation.productionActive = false;
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(operation, 'production_finalized', { deployId: deploy.id }, this.now()),
      );
      return { status: 'completed', operationId: deploy.id };
    });
  }

  async rollback(
    input: RollbackRequest,
    idempotencyKey: string,
  ): Promise<{ status: 'completed'; operationId?: string }> {
    this.assertKey(idempotencyKey, input.upgradeId, 'rollback');
    const hash = requestHash(input);
    return this.store.withMutationLock(async () => {
      const operation = await this.requireByShadow(input.deploymentId);
      this.assertBinding(operation, input);
      this.assertShadowNotAdopted(operation);
      if (operation.rollbackIdempotencyKey) {
        this.assertActionReplay(
          operation.rollbackIdempotencyKey,
          operation.rollbackRequestHash,
          idempotencyKey,
          hash,
        );
      } else {
        operation.rollbackIdempotencyKey = idempotencyKey;
        operation.rollbackRequestHash = hash;
        operation.rollbackState = 'reserved';
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, 'rollback_reserved', {}, this.now()),
        );
      }

      if (
        !operation.promotionIdempotencyKey &&
        !operation.promotionStartedAt &&
        !operation.promotionDeployId
      ) {
        await this.restoreShadow(operation, 'rollback');
        return this.completeRollbackNoop(operation);
      }
      await this.recoverPromotion(operation);
      if (
        input.promotionOperationId &&
        operation.promotionDeployId &&
        input.promotionOperationId !== operation.promotionDeployId
      ) {
        throw new ControllerError(
          'promotion_operation_mismatch',
          'Rollback does not identify the recorded promotion',
          409,
        );
      }
      if (operation.rollbackState === 'completed') {
        return { status: 'completed', operationId: operation.rollbackDeployId ?? undefined };
      }

      if (!operation.promotionDeployId) {
        return this.completeRollbackNoop(operation);
      }
      if (!operation.productionBaselineDeployId || !operation.productionBaselineArtifactSha256) {
        throw internalState('Production rollback baseline is missing');
      }
      const promotion = await this.assertPromotionEvidence(operation);

      if (operation.rollbackDeployId) return this.finishRollback(operation);
      if (operation.rollbackState === 'triggering') {
        const recovered = await this.reconcile(operation, 'rollback');
        operation.rollbackDeployId = recovered.id;
        operation.rollbackState = 'triggered';
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, 'rollback_reconciled', { deployId: recovered.id }, this.now()),
        );
        return this.finishRollback(operation);
      }

      const current = await this.currentLive(this.config.productionServiceId);
      if (
        current.id === operation.productionBaselineDeployId &&
        deployArtifactSha256(current) === operation.productionBaselineArtifactSha256
      ) {
        if (deploymentHealth(promotion) === 'unhealthy') {
          return this.completeRollbackNoop(operation);
        }
        if (deploymentHealth(promotion) === 'healthy') {
          throw new ControllerError(
            'production_drift',
            'A live promotion is not the active production deployment',
            409,
          );
        }
      } else if (
        current.id !== operation.promotionDeployId ||
        !this.matchesArtifact(current, operation)
      ) {
        throw new ControllerError(
          'production_drift',
          'Production changed outside the recorded operation',
          409,
        );
      }

      operation.rollbackState = 'triggering';
      operation.rollbackStartedAt = this.now();
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(operation, 'rollback_triggering', {}, this.now()),
      );
      await this.assertMutationTarget(operation, 'rollback');
      const rollback = await this.render.rollback(
        this.config.productionServiceId,
        operation.productionBaselineDeployId,
      );
      this.assertArtifact(rollback, operation.productionBaselineArtifactSha256);
      operation.rollbackDeployId = rollback.id;
      operation.rollbackState = 'triggered';
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(operation, 'rollback_triggered', { deployId: rollback.id }, this.now()),
      );
      return this.finishRollback(operation);
    });
  }

  private async evaluateShadowHealth(
    operation: DeploymentOperation,
    restoreOnFailure: boolean,
  ): Promise<ReturnType<typeof shadowEvidence>> {
    if (!operation.shadowDeployId) throw internalState('Shadow deploy receipt is missing');
    const deploy = await this.render.deployment(
      this.config.shadowServiceId,
      operation.shadowDeployId,
    );
    if (!this.matchesArtifact(deploy, operation)) {
      return shadowEvidence(operation, 'unhealthy', 'Shadow artifact does not match');
    }
    const health = deploymentHealth(deploy);
    if (health === 'starting') return shadowEvidence(operation, 'starting');
    if (health === 'unhealthy') {
      if (restoreOnFailure) await this.restoreShadow(operation, 'render_failure');
      return shadowEvidence(operation, 'unhealthy', `Render deploy status is ${deploy.status}`);
    }
    const current = await this.currentLive(this.config.shadowServiceId);
    if (current.id !== deploy.id) {
      if (
        current.id === operation.shadowBaselineDeployId &&
        this.withinReconciliationGrace(deploy)
      ) {
        return shadowEvidence(operation, 'starting');
      }
      return shadowEvidence(operation, 'unhealthy', 'Another shadow deployment is active');
    }
    try {
      await this.assertBoundApplication(this.config.shadowServiceId, deploy);
    } catch (error) {
      if (error instanceof ControllerError && error.retryable) throw error;
      if (restoreOnFailure) await this.restoreShadow(operation, 'application_failure');
      return shadowEvidence(operation, 'unhealthy', 'Application checks failed');
    }
    if (operation.shadowState !== 'completed') {
      operation.shadowState = 'completed';
      operation.updatedAt = this.now();
      await this.store.save(operation, operationEvent(operation, 'shadow_healthy', {}, this.now()));
    }
    return shadowEvidence(operation, 'healthy');
  }

  private async restoreShadow(operation: DeploymentOperation, reason: string): Promise<void> {
    if (operation.shadowRestoreState === 'completed') return;
    if (operation.shadowRestoreDeployId) {
      await this.finishShadowRestore(operation);
      return;
    }
    if (operation.shadowRestoreState === 'triggering') {
      const recovered = await this.reconcile(operation, 'shadow_restore');
      operation.shadowRestoreDeployId = recovered.id;
      operation.shadowRestoreState = 'triggered';
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(
          operation,
          'shadow_restore_reconciled',
          { deployId: recovered.id },
          this.now(),
        ),
      );
      await this.finishShadowRestore(operation);
      return;
    }

    const current = await this.currentLive(this.config.shadowServiceId);
    if (
      current.id === operation.shadowBaselineDeployId &&
      deployArtifactSha256(current) === operation.shadowBaselineArtifactSha256
    ) {
      operation.shadowRestoreState = 'completed';
      operation.shadowActive = false;
      operation.updatedAt = this.now();
      await this.store.save(
        operation,
        operationEvent(operation, 'shadow_restored', { reason, mutation: false }, this.now()),
      );
      return;
    }
    if (!operation.shadowDeployId || current.id !== operation.shadowDeployId) {
      throw new ControllerError('shadow_drift', 'Shadow service changed unexpectedly', 409);
    }

    operation.shadowRestoreState = 'triggering';
    operation.shadowRestoreStartedAt = this.now();
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'shadow_restore_triggering', { reason }, this.now()),
    );
    await this.assertMutationTarget(operation, 'shadow_restore');
    const restore = await this.render.rollback(
      this.config.shadowServiceId,
      operation.shadowBaselineDeployId,
    );
    this.assertArtifact(restore, operation.shadowBaselineArtifactSha256);
    operation.shadowRestoreDeployId = restore.id;
    operation.shadowRestoreState = 'triggered';
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'shadow_restore_triggered', { deployId: restore.id }, this.now()),
    );
    await this.finishShadowRestore(operation);
  }

  private async finishShadowRestore(operation: DeploymentOperation): Promise<void> {
    if (!operation.shadowRestoreDeployId) throw internalState('Shadow restore receipt is missing');
    const deploy = await this.render.deployment(
      this.config.shadowServiceId,
      operation.shadowRestoreDeployId,
    );
    this.assertArtifact(deploy, operation.shadowBaselineArtifactSha256);
    const health = deploymentHealth(deploy);
    if (health === 'starting') {
      throw new ControllerError(
        'shadow_restore_in_progress',
        'Shadow restoration is still in progress',
        503,
        true,
        5,
      );
    }
    if (health === 'unhealthy') {
      throw new ControllerError('shadow_restore_failed', 'Shadow restoration failed', 502);
    }
    const current = await this.currentLive(this.config.shadowServiceId);
    if (current.id !== deploy.id) {
      if (this.withinReconciliationGrace(deploy)) {
        throw new ControllerError(
          'shadow_restore_in_progress',
          'Shadow restoration is still converging',
          503,
          true,
          5,
        );
      }
      throw new ControllerError('shadow_drift', 'Shadow restoration is not active', 409);
    }
    await this.assertBoundApplication(this.config.shadowServiceId, deploy);
    operation.shadowRestoreState = 'completed';
    operation.shadowActive = false;
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'shadow_restored', { mutation: true }, this.now()),
    );
  }

  private async finishRollback(
    operation: DeploymentOperation,
  ): Promise<{ status: 'completed'; operationId: string }> {
    if (!operation.rollbackDeployId || !operation.productionBaselineArtifactSha256) {
      throw internalState('Rollback receipt is missing');
    }
    const deploy = await this.render.deployment(
      this.config.productionServiceId,
      operation.rollbackDeployId,
    );
    this.assertArtifact(deploy, operation.productionBaselineArtifactSha256);
    const health = deploymentHealth(deploy);
    if (health === 'starting') {
      throw new ControllerError(
        'rollback_in_progress',
        'Rollback is still in progress',
        503,
        true,
        5,
      );
    }
    if (health === 'unhealthy') {
      throw new ControllerError('rollback_failed', `Rollback status is ${deploy.status}`, 502);
    }
    const current = await this.currentLive(this.config.productionServiceId);
    if (current.id !== deploy.id) {
      if (this.withinReconciliationGrace(deploy)) {
        throw new ControllerError(
          'rollback_in_progress',
          'Rollback is still converging',
          503,
          true,
          5,
        );
      }
      throw new ControllerError('production_drift', 'Rollback is not active', 409);
    }
    await this.assertBoundApplication(this.config.productionServiceId, deploy);
    operation.rollbackState = 'completed';
    operation.shadowActive = false;
    operation.productionActive = false;
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'rollback_completed', { deployId: deploy.id }, this.now()),
    );
    return { status: 'completed', operationId: deploy.id };
  }

  private async completeRollbackNoop(
    operation: DeploymentOperation,
  ): Promise<{ status: 'completed'; operationId: string }> {
    const current = await this.currentLive(this.config.productionServiceId);
    if (
      operation.productionBaselineDeployId &&
      operation.productionBaselineArtifactSha256 &&
      (current.id !== operation.productionBaselineDeployId ||
        deployArtifactSha256(current) !== operation.productionBaselineArtifactSha256)
    ) {
      throw new ControllerError(
        'promotion_evidence_missing',
        'Promotion evidence is missing while production has changed',
        409,
      );
    }
    operation.rollbackState = 'completed';
    operation.rollbackDeployId = `rollback-noop:${operation.upgradeId}`.slice(0, 128);
    operation.shadowActive = false;
    operation.productionActive = false;
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'rollback_completed', { mutation: false }, this.now()),
    );
    return { status: 'completed', operationId: operation.rollbackDeployId };
  }

  private async ensureImageDeployment(
    operation: DeploymentOperation,
    action: 'shadow' | 'promotion',
  ): Promise<RenderDeploy> {
    const state = action === 'shadow' ? operation.shadowState : operation.promotionState;
    if (state === 'triggering') return this.reconcile(operation, action);

    if (action === 'shadow') {
      operation.shadowState = 'triggering';
      operation.shadowStartedAt = this.now();
    } else {
      operation.promotionState = 'triggering';
      operation.promotionStartedAt = this.now();
    }
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, `${action}_triggering`, {}, this.now()),
    );
    await this.assertMutationTarget(operation, action);
    const serviceId =
      action === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId;
    const deploy = await this.render.deployImage(serviceId, operation.imageRef);
    if (!deploy) {
      throw new ControllerError(
        'render_deploy_queued',
        'Render accepted the deployment without a receipt; reconciliation is required',
        503,
        true,
        5,
      );
    }
    this.assertArtifact(deploy, operation.artifactSha256, operation.imageRef);
    return deploy;
  }

  private async reconcile(
    operation: DeploymentOperation,
    action: ReconcileAction,
  ): Promise<RenderDeploy> {
    const { serviceId, startedAt, artifactSha256, trigger } = reconciliation(
      operation,
      action,
      this.config,
    );
    if (!startedAt) throw internalState('Deployment reconciliation timestamp is missing');
    const after = new Date(startedAt.getTime() - 1_000);
    const candidates = (await this.render.listDeploys(serviceId, after)).filter(
      (deploy) => deploy.trigger === trigger && safeArtifactSha256(deploy) === artifactSha256,
    );
    if (candidates.length === 1) return candidates[0];
    if (candidates.length > 1) {
      throw new ControllerError(
        'deployment_reconciliation_ambiguous',
        'Multiple matching deployments require operator reconciliation',
        409,
      );
    }
    if (this.now().getTime() - startedAt.getTime() < this.config.reconciliationGraceMs) {
      throw new ControllerError(
        'deployment_reconciliation_pending',
        'Deployment reconciliation is pending',
        503,
        true,
        5,
      );
    }
    throw new ControllerError(
      'deployment_reconciliation_unresolved',
      'The deployment mutation has no unique receipt and requires operator reconciliation',
      409,
    );
  }

  private async reconcileForReadiness(
    operation: DeploymentOperation,
    target: Target,
  ): Promise<void> {
    const service = await this.requireService(target);
    const expectedFingerprint =
      target === 'shadow'
        ? operation.shadowServiceFingerprint
        : operation.productionServiceFingerprint;
    const baselineArtifactSha256 =
      target === 'shadow'
        ? operation.shadowBaselineArtifactSha256
        : operation.productionBaselineArtifactSha256;
    if (
      !expectedFingerprint ||
      !matchesServiceFingerprint(
        service,
        expectedFingerprint,
        baselineArtifactSha256
          ? exactImageRef(`${this.config.imageRepository}@sha256:${baselineArtifactSha256}`)
          : undefined,
      )
    ) {
      throw new ControllerError(
        'render_service_drift',
        'Render service configuration changed',
        503,
      );
    }

    const actions: ReconcileAction[] =
      target === 'shadow' ? ['shadow', 'shadow_restore'] : ['promotion', 'rollback'];
    for (const action of actions) {
      if (!isTriggeringWithoutReceipt(operation, action)) continue;
      try {
        const recovered = await this.reconcile(operation, action);
        assignReceipt(operation, action, recovered.id);
        operation.updatedAt = this.now();
        await this.store.save(
          operation,
          operationEvent(operation, `${action}_reconciled`, { deployId: recovered.id }, this.now()),
        );
      } catch (error) {
        if (
          error instanceof ControllerError &&
          error.code === 'deployment_reconciliation_pending'
        ) {
          continue;
        }
        throw error;
      }
    }

    const receipt = activeReceipt(operation, target);
    if (!receipt) {
      await this.assertHealthyLive(
        target === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId,
      );
      return;
    }
    const serviceId =
      target === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId;
    const deploy = await this.render.deployment(serviceId, receipt.id);
    this.assertArtifact(deploy, receipt.artifactSha256);
    if (deploymentHealth(deploy) === 'healthy') {
      const current = await this.currentLive(serviceId);
      if (current.id !== deploy.id && !receipt.restoring) {
        throw new ControllerError(
          'render_service_drift',
          'Active deploy changed unexpectedly',
          503,
        );
      }
      if (current.id === deploy.id) {
        await this.assertBoundApplication(serviceId, deploy);
      }
    }
  }

  private async assertMutationTarget(
    operation: DeploymentOperation,
    action: ReconcileAction,
  ): Promise<void> {
    const target: Target =
      action === 'shadow' || action === 'shadow_restore' ? 'shadow' : 'production';
    const service = await this.requireService(target);
    const expectedFingerprint =
      target === 'shadow'
        ? operation.shadowServiceFingerprint
        : operation.productionServiceFingerprint;
    const baselineArtifactSha256 =
      target === 'shadow'
        ? operation.shadowBaselineArtifactSha256
        : operation.productionBaselineArtifactSha256;
    if (
      !expectedFingerprint ||
      !matchesServiceFingerprint(
        service,
        expectedFingerprint,
        baselineArtifactSha256
          ? exactImageRef(`${this.config.imageRepository}@sha256:${baselineArtifactSha256}`)
          : undefined,
      )
    ) {
      throw new ControllerError(
        'render_service_drift',
        'Render service configuration changed',
        409,
      );
    }
    const current = await this.currentLive(
      target === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId,
    );
    if (
      action === 'rollback' &&
      operation.productionBaselineDeployId &&
      operation.productionBaselineArtifactSha256 &&
      current.id === operation.productionBaselineDeployId &&
      deployArtifactSha256(current) === operation.productionBaselineArtifactSha256
    ) {
      return;
    }
    const expected = mutationBaseline(operation, action);
    if (
      current.id !== expected.deployId ||
      deployArtifactSha256(current) !== expected.artifactSha256
    ) {
      throw new ControllerError(
        'deployment_target_drift',
        'Deployment target changed before mutation',
        409,
      );
    }
  }

  private async recoverPromotion(operation: DeploymentOperation): Promise<void> {
    if (operation.promotionDeployId) return;
    if (!operation.promotionStartedAt) return;
    const recovered = await this.reconcile(operation, 'promotion');
    operation.promotionDeployId = recovered.id;
    operation.promotionState = 'triggered';
    operation.updatedAt = this.now();
    await this.store.save(
      operation,
      operationEvent(operation, 'promotion_reconciled', { deployId: recovered.id }, this.now()),
    );
  }

  private async assertPromotionEvidence(operation: DeploymentOperation): Promise<RenderDeploy> {
    if (!operation.promotionDeployId || !operation.promotionStartedAt) {
      throw internalState('Promotion evidence is incomplete');
    }
    const deploy = await this.render.deployment(
      this.config.productionServiceId,
      operation.promotionDeployId,
    );
    this.assertArtifact(deploy, operation.artifactSha256, operation.imageRef);
    if (new Date(deploy.createdAt).getTime() < operation.promotionStartedAt.getTime() - 1_000) {
      throw new ControllerError(
        'promotion_evidence_invalid',
        'Promotion receipt predates the recorded mutation',
        409,
      );
    }
    return deploy;
  }

  private async releasePriorProduction(upgradeId: string): Promise<void> {
    const active = await this.store.activeProduction();
    if (!active || active.upgradeId === upgradeId) return;
    throw new ControllerError(
      'production_busy',
      'The previous production promotion has not been finalized',
      503,
      true,
      5,
    );
  }

  private async findExisting(
    upgradeId: string,
    proposalId: string,
    idempotencyKey: string,
  ): Promise<DeploymentOperation | null> {
    const matches = await Promise.all([
      this.store.get(upgradeId),
      this.store.getByProposal(proposalId),
      this.store.getByIdempotency(idempotencyKey),
    ]);
    const distinct = new Map(
      matches
        .filter((value): value is DeploymentOperation => value !== null)
        .map((value) => [value.upgradeId, value]),
    );
    if (distinct.size > 1) {
      throw new ControllerError(
        'operation_lookup_conflict',
        'Operation identifiers resolve to different records',
        409,
      );
    }
    return distinct.values().next().value ?? null;
  }

  private async requireByShadow(deploymentId: string): Promise<DeploymentOperation> {
    const operation = await this.store.getByShadowDeploy(deploymentId);
    if (!operation) {
      throw new ControllerError('deployment_not_found', 'Deployment was not found', 404);
    }
    return operation;
  }

  private async requireService(target: Target): Promise<RenderService> {
    const service = await this.render.service(
      target === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId,
    );
    this.assertService(service, target);
    return service;
  }

  private assertService(service: RenderService, target: Target): void {
    const expectedId =
      target === 'shadow' ? this.config.shadowServiceId : this.config.productionServiceId;
    const expectedType = target === 'shadow' ? 'private_service' : 'web_service';
    if (service.id !== expectedId || service.type !== expectedType) {
      throw new ControllerError('render_service_mismatch', 'Render service does not match', 503);
    }
    if (service.repo || serviceImageRepository(service.imagePath) !== this.config.imageRepository) {
      throw new ControllerError(
        'artifact_execution_unbound',
        'Render target is not bound to the approved image repository',
        503,
      );
    }
  }

  private assertRepository(repository: string): void {
    if (repository.toLowerCase() !== this.config.repository) {
      throw new ControllerError('repository_denied', 'Repository is not allowed', 403);
    }
  }

  private assertShadowReplay(
    operation: DeploymentOperation,
    input: ShadowRequest,
    idempotencyKey: string,
    hash: string,
  ): void {
    if (
      operation.upgradeId !== input.upgradeId ||
      operation.proposalId !== input.proposalId ||
      operation.shadowIdempotencyKey !== idempotencyKey ||
      operation.shadowRequestHash !== hash
    ) {
      throw new ControllerError(
        'idempotency_conflict',
        'Shadow operation was already reserved for different content',
        409,
      );
    }
  }

  private assertBinding(
    operation: DeploymentOperation,
    input: PromotionRequest | FinalizeRequest | RollbackRequest,
  ): void {
    if (
      operation.upgradeId !== input.upgradeId ||
      operation.proposalId !== input.proposalId ||
      operation.shadowDeployId !== input.deploymentId ||
      operation.candidateSha !== input.candidateSha
    ) {
      throw new ControllerError(
        'operation_binding_mismatch',
        'Request does not match the recorded deployment operation',
        409,
      );
    }
  }

  private assertShadowAdoptionBinding(
    operation: DeploymentOperation,
    input: ShadowAdoptionRequest,
  ): void {
    if (
      operation.upgradeId !== input.upgradeId ||
      operation.proposalId !== input.proposalId ||
      operation.shadowDeployId !== input.deploymentId ||
      operation.shadowRestoreDeployId !== input.restoreDeploymentId ||
      operation.candidateSha !== input.candidateSha ||
      operation.artifactSha256 !== input.candidateArtifactSha256 ||
      operation.shadowBaselineDeployId !== input.baselineDeploymentId ||
      operation.shadowBaselineArtifactSha256 !== input.baselineArtifactSha256
    ) {
      throw new ControllerError(
        'operation_binding_mismatch',
        'Request does not match the recorded deployment operation',
        409,
      );
    }
  }

  private assertShadowAdoptionState(operation: DeploymentOperation): void {
    if (
      !operation.shadowActive ||
      operation.shadowState !== 'completed' ||
      operation.shadowRestoreState !== 'triggered' ||
      !operation.shadowDeployId ||
      !operation.shadowRestoreDeployId
    ) {
      throw new ControllerError(
        'shadow_adoption_not_allowed',
        'Shadow adoption requires a healthy candidate and a failed restore in progress',
        409,
      );
    }
    if (
      operation.promotionIdempotencyKey ||
      operation.promotionRequestHash ||
      operation.promotionState ||
      operation.mergeSha ||
      operation.productionServiceFingerprint ||
      operation.productionBaselineDeployId ||
      operation.productionBaselineArtifactSha256 ||
      operation.promotionStartedAt ||
      operation.promotionDeployId ||
      operation.productionActive ||
      operation.productionFinalizedAt
    ) {
      throw new ControllerError(
        'shadow_adoption_not_allowed',
        'Shadow cannot be adopted after production promotion has started',
        409,
      );
    }
  }

  private assertShadowNotAdopted(operation: DeploymentOperation): void {
    if (operation.shadowRestoreState !== 'failed' || operation.shadowActive) return;
    throw new ControllerError(
      'shadow_baseline_adopted',
      'The failed operation is closed; submit a new deployment operation',
      409,
    );
  }

  private assertActionReplay(
    expectedKey: string,
    expectedHash: string | null,
    actualKey: string,
    actualHash: string,
  ): void {
    if (expectedKey !== actualKey || expectedHash !== actualHash) {
      throw new ControllerError(
        'idempotency_conflict',
        'Operation was already reserved for different content',
        409,
      );
    }
  }

  private assertKey(key: string, upgradeId: string, action: string): void {
    if (key !== `${upgradeId}:${action}`) {
      throw new ControllerError(
        'idempotency_key_mismatch',
        'Idempotency key does not match the operation',
        400,
      );
    }
  }

  private matchesArtifact(deploy: RenderDeploy, operation: DeploymentOperation): boolean {
    return (
      safeArtifactSha256(deploy) === operation.artifactSha256 &&
      deploy.image?.ref === operation.imageRef
    );
  }

  private assertArtifact(deploy: RenderDeploy, artifactSha256: string, imageRef?: string): void {
    if (
      safeArtifactSha256(deploy) !== artifactSha256 ||
      (imageRef !== undefined && deploy.image?.ref !== imageRef)
    ) {
      throw new ControllerError(
        'artifact_execution_mismatch',
        'Render deployment does not run the approved artifact digest',
        502,
      );
    }
  }

  private async currentLive(serviceId: string): Promise<RenderDeploy> {
    const live = (await this.render.listDeploys(serviceId)).filter(
      (deploy) => deploy.status === 'live',
    );
    if (live.length !== 1) {
      throw new ControllerError(
        'render_live_deploy_ambiguous',
        'Render service does not have exactly one live deploy',
        503,
        true,
        5,
      );
    }
    const deploy = live[0];
    deployArtifactSha256(deploy);
    if (
      !deploy.image ||
      serviceImageRepository(deploy.image.ref) !== this.config.imageRepository ||
      deploy.image.ref !== `${this.config.imageRepository}@sha256:${deployArtifactSha256(deploy)}`
    ) {
      throw new ControllerError(
        'artifact_execution_unbound',
        'Live deployment is not pinned to its immutable image digest',
        503,
      );
    }
    return deploy;
  }

  private async assertHealthyLive(serviceId: string): Promise<RenderDeploy> {
    const deploy = await this.currentLive(serviceId);
    await this.assertBoundApplication(serviceId, deploy);
    return deploy;
  }

  private withinReconciliationGrace(deploy: RenderDeploy): boolean {
    return (
      this.now().getTime() - new Date(deploy.createdAt).getTime() <
      this.config.reconciliationGraceMs
    );
  }

  private async assertBoundApplication(serviceId: string, expected: RenderDeploy): Promise<void> {
    const artifactSha256 = deployArtifactSha256(expected);
    const before = await this.currentLive(serviceId);
    if (before.id !== expected.id || deployArtifactSha256(before) !== artifactSha256) {
      throw new ControllerError(
        'application_probe_deploy_drift',
        'Active deployment changed before application probing',
        409,
      );
    }
    await this.applications.probe(serviceId);
    const after = await this.currentLive(serviceId);
    if (after.id !== expected.id || deployArtifactSha256(after) !== artifactSha256) {
      throw new ControllerError(
        'application_probe_deploy_drift',
        'Active deployment changed during application probing',
        409,
      );
    }
  }
}

function newOperation(
  input: ShadowRequest,
  idempotencyKey: string,
  hash: string,
  imageRepository: string,
  shadowServiceFingerprint: string,
  shadowBaseline: RenderDeploy,
  now: Date,
): DeploymentOperation {
  const imageRef = exactImageRef(`${imageRepository}@sha256:${input.artifact.sha256}`);
  return {
    upgradeId: input.upgradeId,
    proposalId: input.proposalId,
    repository: `${input.repository.owner}/${input.repository.name}`.toLowerCase(),
    manifestSha256: input.manifestSha256,
    candidateSha: input.candidateSha,
    artifactUrl: input.artifact.url,
    artifactSha256: input.artifact.sha256,
    artifactSizeBytes: input.artifact.sizeBytes,
    imageRef,
    artifactVerifiedAt: null,
    prNumber: input.prNumber,
    shadowIdempotencyKey: idempotencyKey,
    shadowRequestHash: hash,
    shadowState: 'reserved',
    shadowServiceFingerprint,
    shadowBaselineDeployId: shadowBaseline.id,
    shadowBaselineArtifactSha256: deployArtifactSha256(shadowBaseline),
    shadowStartedAt: null,
    shadowDeployId: null,
    shadowActive: true,
    shadowRestoreState: null,
    shadowRestoreStartedAt: null,
    shadowRestoreDeployId: null,
    promotionIdempotencyKey: null,
    promotionRequestHash: null,
    promotionState: null,
    mergeSha: null,
    productionServiceFingerprint: null,
    productionBaselineDeployId: null,
    productionBaselineArtifactSha256: null,
    promotionStartedAt: null,
    promotionDeployId: null,
    productionActive: false,
    productionFinalizedAt: null,
    rollbackIdempotencyKey: null,
    rollbackRequestHash: null,
    rollbackState: null,
    rollbackStartedAt: null,
    rollbackDeployId: null,
    createdAt: now,
    updatedAt: now,
  };
}

function reconciliation(
  operation: DeploymentOperation,
  action: ReconcileAction,
  config: Pick<ControllerConfig, 'shadowServiceId' | 'productionServiceId'>,
) {
  switch (action) {
    case 'shadow':
      return {
        serviceId: config.shadowServiceId,
        startedAt: operation.shadowStartedAt,
        artifactSha256: operation.artifactSha256,
        trigger: 'api' as const,
      };
    case 'promotion':
      return {
        serviceId: config.productionServiceId,
        startedAt: operation.promotionStartedAt,
        artifactSha256: operation.artifactSha256,
        trigger: 'api' as const,
      };
    case 'shadow_restore':
      return {
        serviceId: config.shadowServiceId,
        startedAt: operation.shadowRestoreStartedAt,
        artifactSha256: operation.shadowBaselineArtifactSha256,
        trigger: 'rollback' as const,
      };
    case 'rollback':
      if (!operation.productionBaselineArtifactSha256) {
        throw internalState('Production rollback baseline is missing');
      }
      return {
        serviceId: config.productionServiceId,
        startedAt: operation.rollbackStartedAt,
        artifactSha256: operation.productionBaselineArtifactSha256,
        trigger: 'rollback' as const,
      };
  }
}

function mutationBaseline(operation: DeploymentOperation, action: ReconcileAction) {
  switch (action) {
    case 'shadow':
      return {
        deployId: operation.shadowBaselineDeployId,
        artifactSha256: operation.shadowBaselineArtifactSha256,
      };
    case 'promotion':
      if (!operation.productionBaselineDeployId || !operation.productionBaselineArtifactSha256) {
        throw internalState('Production baseline is missing');
      }
      return {
        deployId: operation.productionBaselineDeployId,
        artifactSha256: operation.productionBaselineArtifactSha256,
      };
    case 'shadow_restore':
      if (!operation.shadowDeployId) throw internalState('Shadow deploy is missing');
      return { deployId: operation.shadowDeployId, artifactSha256: operation.artifactSha256 };
    case 'rollback':
      if (!operation.promotionDeployId) throw internalState('Promotion deploy is missing');
      return { deployId: operation.promotionDeployId, artifactSha256: operation.artifactSha256 };
  }
}

function isTriggeringWithoutReceipt(
  operation: DeploymentOperation,
  action: ReconcileAction,
): boolean {
  switch (action) {
    case 'shadow':
      return operation.shadowState === 'triggering' && !operation.shadowDeployId;
    case 'promotion':
      return operation.promotionState === 'triggering' && !operation.promotionDeployId;
    case 'shadow_restore':
      return operation.shadowRestoreState === 'triggering' && !operation.shadowRestoreDeployId;
    case 'rollback':
      return operation.rollbackState === 'triggering' && !operation.rollbackDeployId;
  }
}

function assignReceipt(
  operation: DeploymentOperation,
  action: ReconcileAction,
  deployId: string,
): void {
  switch (action) {
    case 'shadow':
      operation.shadowDeployId = deployId;
      operation.shadowState = 'triggered';
      break;
    case 'promotion':
      operation.promotionDeployId = deployId;
      operation.promotionState = 'triggered';
      break;
    case 'shadow_restore':
      operation.shadowRestoreDeployId = deployId;
      operation.shadowRestoreState = 'triggered';
      break;
    case 'rollback':
      operation.rollbackDeployId = deployId;
      operation.rollbackState = 'triggered';
      break;
  }
}

function activeReceipt(operation: DeploymentOperation, target: Target) {
  if (target === 'shadow') {
    if (operation.shadowRestoreDeployId) {
      return {
        id: operation.shadowRestoreDeployId,
        artifactSha256: operation.shadowBaselineArtifactSha256,
        restoring: true,
      };
    }
    if (operation.shadowDeployId) {
      return {
        id: operation.shadowDeployId,
        artifactSha256: operation.artifactSha256,
        restoring: false,
      };
    }
    return null;
  }
  if (operation.rollbackDeployId && operation.productionBaselineArtifactSha256) {
    return {
      id: operation.rollbackDeployId,
      artifactSha256: operation.productionBaselineArtifactSha256,
      restoring: true,
    };
  }
  if (operation.promotionDeployId) {
    return {
      id: operation.promotionDeployId,
      artifactSha256: operation.artifactSha256,
      restoring: false,
    };
  }
  return null;
}

function serviceFingerprint(service: RenderService): string {
  return requestHash({
    id: service.id,
    type: service.type,
    autoDeploy: service.autoDeploy,
    imageRepository: serviceImageRepository(service.imagePath),
    registryCredential: service.registryCredential ?? null,
    runtime: service.serviceDetails.runtime,
    region: service.serviceDetails.region,
    numInstances: service.serviceDetails.numInstances ?? null,
    url: service.serviceDetails.url ?? null,
    suspended: service.suspended,
  });
}

function matchesServiceFingerprint(
  service: RenderService,
  expected: string,
  legacyImagePath?: string,
): boolean {
  if (serviceFingerprint(service) === expected) return true;
  if (legacyServiceFingerprint(service) === expected) return true;
  if (!legacyImagePath) return false;
  return legacyServiceFingerprint({ ...service, imagePath: legacyImagePath }) === expected;
}

function legacyServiceFingerprint(service: RenderService): string {
  return requestHash({
    id: service.id,
    type: service.type,
    autoDeploy: service.autoDeploy,
    imagePath: service.imagePath,
    registryCredential: service.registryCredential ?? null,
    runtime: service.serviceDetails.runtime,
    region: service.serviceDetails.region,
    numInstances: service.serviceDetails.numInstances ?? null,
    url: service.serviceDetails.url ?? null,
    suspended: service.suspended,
  });
}

function safeArtifactSha256(deploy: RenderDeploy): string | null {
  try {
    return deployArtifactSha256(deploy);
  } catch {
    return null;
  }
}

function shadowEvidence(
  operation: DeploymentOperation,
  status: 'starting' | 'healthy' | 'unhealthy',
  detail?: string,
) {
  return {
    status,
    candidateSha: operation.candidateSha,
    environment: 'shadow' as const,
    ...(detail ? { detail } : {}),
  };
}

function promotionEvidence(
  operation: DeploymentOperation,
  status: 'starting' | 'healthy' | 'unhealthy',
  active: boolean,
  detail?: string,
) {
  if (!operation.mergeSha || !operation.promotionDeployId) {
    throw internalState('Promotion evidence is incomplete');
  }
  return {
    status,
    candidateSha: operation.candidateSha,
    environment: 'production' as const,
    active,
    mergeSha: operation.mergeSha,
    promotionOperationId: operation.promotionDeployId,
    ...(detail ? { detail } : {}),
  };
}

function internalState(message: string): ControllerError {
  return new ControllerError('operation_state_invalid', message, 500);
}
