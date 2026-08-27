import { createHash } from 'node:crypto';
import { z } from 'zod';

export const externalId = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
export const gitSha = z.string().regex(/^[a-f0-9]{40}$/);
export const sha256 = z.string().regex(/^[a-f0-9]{64}$/);
export const ociDigest = z.string().regex(/^sha256:[a-f0-9]{64}$/);
const githubName = z
  .string()
  .min(1)
  .max(100)
  .regex(/^[A-Za-z0-9_.-]+$/);
const branch = z
  .string()
  .min(1)
  .max(240)
  .regex(/^[A-Za-z0-9._/-]+$/)
  .refine(
    (value) =>
      !value.startsWith('/') &&
      !value.endsWith('/') &&
      !value.endsWith('.') &&
      !value.includes('..'),
    'Invalid Git branch name',
  );
const artifact = z
  .object({
    url: z.string().url(),
    sha256,
    sizeBytes: z
      .number()
      .int()
      .positive()
      .max(4 * 1024 * 1024),
  })
  .strict();

export const shadowRequestSchema = z
  .object({
    version: z.literal(1),
    upgradeId: externalId,
    proposalId: externalId,
    manifestSha256: sha256,
    repository: z
      .object({
        owner: githubName,
        name: githubName,
        baseBranch: branch,
        headBranch: branch,
      })
      .strict(),
    candidateSha: gitSha,
    artifact,
    prNumber: z.number().int().positive(),
  })
  .strict();

export const promotionRequestSchema = z
  .object({
    version: z.literal(1),
    upgradeId: externalId,
    proposalId: externalId,
    deploymentId: externalId,
    candidateSha: gitSha,
    mergeSha: gitSha,
  })
  .strict();

export const rollbackRequestSchema = z
  .object({
    version: z.literal(1),
    upgradeId: externalId,
    proposalId: externalId,
    deploymentId: externalId,
    candidateSha: gitSha,
    promotionOperationId: externalId.optional(),
    reason: z.string().trim().min(1).max(500),
  })
  .strict();

export const shadowAdoptionRequestSchema = z
  .object({
    version: z.literal(1),
    upgradeId: externalId,
    proposalId: externalId,
    deploymentId: externalId,
    restoreDeploymentId: externalId,
    candidateSha: gitSha,
    candidateArtifactSha256: sha256,
    baselineDeploymentId: externalId,
    baselineArtifactSha256: sha256,
    reason: z.literal('schema_incompatible_baseline'),
  })
  .strict();

export const finalizeRequestSchema = z
  .object({
    version: z.literal(1),
    upgradeId: externalId,
    proposalId: externalId,
    deploymentId: externalId,
    candidateSha: gitSha,
    mergeSha: gitSha,
    promotionOperationId: externalId,
  })
  .strict();

export type ShadowRequest = z.infer<typeof shadowRequestSchema>;
export type PromotionRequest = z.infer<typeof promotionRequestSchema>;
export type FinalizeRequest = z.infer<typeof finalizeRequestSchema>;
export type RollbackRequest = z.infer<typeof rollbackRequestSchema>;
export type ShadowAdoptionRequest = z.infer<typeof shadowAdoptionRequestSchema>;
export type ActionState = 'reserved' | 'triggering' | 'triggered' | 'completed' | 'failed';

export interface DeploymentOperation {
  upgradeId: string;
  proposalId: string;
  repository: string;
  manifestSha256: string;
  candidateSha: string;
  artifactUrl: string;
  artifactSha256: string;
  artifactSizeBytes: number;
  imageRef: string;
  artifactVerifiedAt: Date | null;
  prNumber: number;
  shadowIdempotencyKey: string;
  shadowRequestHash: string;
  shadowState: ActionState;
  shadowServiceFingerprint: string;
  shadowBaselineDeployId: string;
  shadowBaselineArtifactSha256: string;
  shadowStartedAt: Date | null;
  shadowDeployId: string | null;
  shadowActive: boolean;
  shadowRestoreState: ActionState | null;
  shadowRestoreStartedAt: Date | null;
  shadowRestoreDeployId: string | null;
  promotionIdempotencyKey: string | null;
  promotionRequestHash: string | null;
  promotionState: ActionState | null;
  mergeSha: string | null;
  productionServiceFingerprint: string | null;
  productionBaselineDeployId: string | null;
  productionBaselineArtifactSha256: string | null;
  promotionStartedAt: Date | null;
  promotionDeployId: string | null;
  productionActive: boolean;
  productionFinalizedAt: Date | null;
  rollbackIdempotencyKey: string | null;
  rollbackRequestHash: string | null;
  rollbackState: ActionState | null;
  rollbackStartedAt: Date | null;
  rollbackDeployId: string | null;
  createdAt: Date;
  updatedAt: Date;
}

export interface OperationEvent {
  operationId: string;
  type: string;
  recordSha256: string;
  detail: Record<string, string | number | boolean | null>;
  createdAt: Date;
}

export class ControllerError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status = 422,
    readonly retryable = false,
    readonly retryAfterSeconds?: number,
  ) {
    super(message);
  }
}

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') {
    return JSON.stringify(value);
  }
  if (value instanceof Date) return JSON.stringify(value.toISOString());
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error('Canonical JSON requires finite numbers');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (typeof value !== 'object') throw new Error('Unsupported canonical JSON value');
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object)
    .filter((key) => object[key] !== undefined)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key])}`)
    .join(',')}}`;
}

export function requestHash(value: unknown): string {
  return createHash('sha256').update(canonicalJson(value)).digest('hex');
}

export function operationEvent(
  operation: DeploymentOperation,
  type: string,
  detail: OperationEvent['detail'] = {},
  now = new Date(),
): OperationEvent {
  return {
    operationId: operation.upgradeId,
    type,
    recordSha256: requestHash(operation),
    detail,
    createdAt: now,
  };
}
