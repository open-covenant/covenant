import { z } from 'zod';
import type { UpgradeManifest } from './domain.js';
import { UpdaterError } from './domain.js';

const operationId = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const shadowResponseSchema = z.object({ deploymentId: operationId }).strict();
const promotionResponseSchema = z.object({ status: z.literal('completed'), operationId }).strict();
const rollbackResponseSchema = z
  .object({ status: z.literal('completed'), operationId: operationId.optional() })
  .strict();
const healthStatusSchema = z.enum(['starting', 'healthy', 'unhealthy']);
const gitSha = z.string().regex(/^[a-f0-9]{40}$/);
const shadowHealthResponseSchema = z
  .object({
    status: healthStatusSchema,
    candidateSha: gitSha,
    environment: z.literal('shadow'),
    detail: z.string().max(1_000).optional(),
  })
  .strict();
const promotionHealthResponseSchema = z
  .object({
    status: healthStatusSchema,
    candidateSha: gitSha,
    environment: z.literal('production'),
    active: z.boolean(),
    mergeSha: gitSha,
    promotionOperationId: operationId,
    detail: z.string().max(1_000).optional(),
  })
  .strict();

export interface ShadowReceipt {
  deploymentId: string;
}

export interface DeploymentHealth {
  status: 'starting' | 'healthy' | 'unhealthy';
  detail?: string;
}

export interface PromotionReceipt {
  operationId: string;
}

export interface PromotionHealth extends DeploymentHealth {
  active: boolean;
  mergeSha: string;
  operationId: string;
}

export interface DeploymentGateway {
  startShadow(
    upgradeId: string,
    manifest: UpgradeManifest,
    manifestHash: string,
    prNumber: number,
  ): Promise<ShadowReceipt>;
  shadowHealth(deploymentId: string, candidateSha: string): Promise<DeploymentHealth>;
  promotionHealth(
    deploymentId: string,
    candidateSha: string,
    mergeSha: string,
    operationId: string,
  ): Promise<PromotionHealth>;
  promote(
    upgradeId: string,
    deploymentId: string,
    manifest: UpgradeManifest,
    mergeSha: string,
  ): Promise<PromotionReceipt>;
  rollback(
    upgradeId: string,
    deploymentId: string,
    manifest: UpgradeManifest,
    reason: string,
    promotionOperationId: string | null,
  ): Promise<void>;
}

export interface DeploymentHookConfig {
  shadowUrl: string;
  shadowHealthUrlTemplate: string;
  promotionHealthUrlTemplate: string;
  promoteUrl: string;
  rollbackUrl: string;
  token: string;
  timeoutMs: number;
}

export class HttpDeploymentGateway implements DeploymentGateway {
  constructor(private readonly config: DeploymentHookConfig) {}

  async startShadow(
    upgradeId: string,
    manifest: UpgradeManifest,
    manifestHash: string,
    prNumber: number,
  ): Promise<ShadowReceipt> {
    const payload = await this.post(this.config.shadowUrl, `${upgradeId}:shadow`, {
      version: 1,
      upgradeId,
      proposalId: manifest.proposalId,
      manifestSha256: manifestHash,
      repository: manifest.repository,
      candidateSha: manifest.candidateSha,
      artifact: manifest.artifact,
      prNumber,
    });
    return shadowResponseSchema.parse(payload);
  }

  async shadowHealth(deploymentId: string, candidateSha: string): Promise<DeploymentHealth> {
    const url = this.config.shadowHealthUrlTemplate.replace(
      '{deploymentId}',
      encodeURIComponent(deploymentId),
    );
    const payload = shadowHealthResponseSchema.parse(await this.request(url, { method: 'GET' }));
    if (payload.candidateSha !== candidateSha) {
      throw new UpdaterError('health_commit_mismatch', 'Health response covers another commit');
    }
    return { status: payload.status, detail: payload.detail };
  }

  async promotionHealth(
    deploymentId: string,
    candidateSha: string,
    mergeSha: string,
    operationId: string,
  ): Promise<PromotionHealth> {
    const url = this.config.promotionHealthUrlTemplate.replace(
      '{deploymentId}',
      encodeURIComponent(deploymentId),
    );
    const payload = promotionHealthResponseSchema.parse(await this.request(url, { method: 'GET' }));
    if (payload.candidateSha !== candidateSha) {
      throw new UpdaterError(
        'promotion_commit_mismatch',
        'Production health covers another candidate commit',
      );
    }
    if (payload.mergeSha !== mergeSha) {
      throw new UpdaterError(
        'promotion_merge_mismatch',
        'Production health covers another merge commit',
      );
    }
    if (payload.promotionOperationId !== operationId) {
      throw new UpdaterError(
        'promotion_operation_mismatch',
        'Production health covers another promotion operation',
      );
    }
    if (payload.status === 'healthy' && !payload.active) {
      throw new UpdaterError(
        'promotion_not_active',
        'Production health is healthy but the candidate is not active',
      );
    }
    return {
      status: payload.status,
      detail: payload.detail,
      active: payload.active,
      mergeSha: payload.mergeSha,
      operationId: payload.promotionOperationId,
    };
  }

  async promote(
    upgradeId: string,
    deploymentId: string,
    manifest: UpgradeManifest,
    mergeSha: string,
  ): Promise<PromotionReceipt> {
    const payload = promotionResponseSchema.parse(
      await this.post(this.config.promoteUrl, `${upgradeId}:promote`, {
        version: 1,
        upgradeId,
        proposalId: manifest.proposalId,
        deploymentId,
        candidateSha: manifest.candidateSha,
        mergeSha,
      }),
    );
    return { operationId: payload.operationId };
  }

  async rollback(
    upgradeId: string,
    deploymentId: string,
    manifest: UpgradeManifest,
    reason: string,
    promotionOperationId: string | null,
  ): Promise<void> {
    rollbackResponseSchema.parse(
      await this.post(this.config.rollbackUrl, `${upgradeId}:rollback`, {
        version: 1,
        upgradeId,
        proposalId: manifest.proposalId,
        deploymentId,
        candidateSha: manifest.candidateSha,
        ...(promotionOperationId ? { promotionOperationId } : {}),
        reason: reason.slice(0, 500),
      }),
    );
  }

  private post(
    url: string,
    idempotencyKey: string,
    body: Record<string, unknown>,
  ): Promise<unknown> {
    return this.request(url, { method: 'POST', idempotencyKey, body });
  }

  private async request(
    url: string,
    options: { method: 'GET' | 'POST'; idempotencyKey?: string; body?: Record<string, unknown> },
  ): Promise<unknown> {
    let response: Response;
    try {
      response = await fetch(url, {
        method: options.method,
        headers: {
          accept: 'application/json',
          authorization: `Bearer ${this.config.token}`,
          'cache-control': 'no-store',
          ...(options.body ? { 'content-type': 'application/json' } : {}),
          ...(options.idempotencyKey ? { 'idempotency-key': options.idempotencyKey } : {}),
        },
        body: options.body ? JSON.stringify(options.body) : undefined,
        redirect: 'error',
        signal: AbortSignal.timeout(this.config.timeoutMs),
      });
    } catch (error) {
      throw new UpdaterError(
        'deployment_unavailable',
        error instanceof Error ? error.message : 'Deployment endpoint failed',
        503,
        true,
      );
    }
    const text = await response.text();
    let payload: unknown = null;
    if (text) {
      try {
        payload = JSON.parse(text);
      } catch {
        throw new UpdaterError(
          'deployment_invalid_response',
          'Deployment endpoint returned invalid JSON',
          502,
          response.status >= 500,
        );
      }
    }
    if (!response.ok) {
      const retryable =
        response.status === 408 || response.status === 429 || response.status >= 500;
      throw new UpdaterError(
        'deployment_request_failed',
        readMessage(payload) ?? `Deployment endpoint returned ${response.status}`,
        retryable ? 503 : 422,
        retryable,
      );
    }
    return payload;
  }
}

function readMessage(value: unknown): string | null {
  if (!value || typeof value !== 'object' || !('message' in value)) return null;
  return typeof value.message === 'string' ? value.message.slice(0, 500) : null;
}
