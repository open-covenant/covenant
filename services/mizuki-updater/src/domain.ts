import { createHash, randomUUID } from 'node:crypto';
import { z } from 'zod';

const githubName = z
  .string()
  .min(1)
  .max(100)
  .regex(/^[A-Za-z0-9_.-]+$/);
const branch = z
  .string()
  .min(1)
  .max(240)
  .refine(
    (value) =>
      !value.startsWith('/') &&
      !value.endsWith('/') &&
      !value.endsWith('.') &&
      !value.includes('..') &&
      !/[\s~^:?*[\\]/.test(value),
    'Invalid Git branch name',
  );
const sha256 = z.string().regex(/^[a-f0-9]{64}$/);
const gitSha = z.string().regex(/^[a-f0-9]{40}$/);
const externalId = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const routeName = z
  .string()
  .min(1)
  .max(200)
  .regex(/^[A-Za-z0-9][A-Za-z0-9./:_-]*$/);
const ed25519Signature = z
  .string()
  .min(80)
  .max(128)
  .regex(/^[A-Za-z0-9+/]+={0,2}$/);
const httpsUrl = z
  .string()
  .url()
  .refine((value) => {
    const url = new URL(value);
    return url.protocol === 'https:' && !url.username && !url.password;
  }, 'HTTPS without embedded credentials is required');

export const benchmarkReceiptSchema = z
  .object({
    version: z.literal(1),
    receiptId: externalId,
    candidateSha: gitSha,
    artifactSha256: sha256,
    suite: z.string().min(1).max(200),
    targetMetric: z.string().min(1).max(200),
    direction: z.enum(['increase', 'decrease']),
    baseline: z.number().finite(),
    candidate: z.number().finite(),
    minimumImprovement: z.number().finite().positive(),
    protectedSuitePassed: z.literal(true),
    completedAt: z.string().datetime({ offset: true }),
  })
  .strict();

export const reviewReceiptSchema = z
  .object({
    version: z.literal(1),
    receiptId: externalId,
    candidateSha: gitSha,
    artifactSha256: sha256,
    implementerRoute: routeName,
    reviewerRoute: routeName,
    verdict: z.literal('approved'),
    blockingFindings: z.literal(0),
    summary: z.string().min(1).max(4_000),
    completedAt: z.string().datetime({ offset: true }),
  })
  .strict();

export const upgradeManifestSchema = z
  .object({
    version: z.literal(1),
    proposalId: externalId,
    sourceHandoffSha256: sha256,
    repository: z
      .object({
        owner: githubName,
        name: githubName,
        baseBranch: branch,
        baseSha: gitSha,
        headBranch: branch,
      })
      .strict(),
    candidateSha: gitSha,
    artifact: z
      .object({
        url: httpsUrl,
        sha256,
        sizeBytes: z
          .number()
          .int()
          .positive()
          .max(100 * 1024 * 1024),
      })
      .strict(),
    title: z.string().min(1).max(256),
    body: z.string().min(1).max(20_000),
    requiredChecks: z.array(z.string().min(1).max(200)).min(1).max(50),
    benchmark: z
      .object({
        receipt: benchmarkReceiptSchema,
        sha256,
        keyId: externalId,
        signature: ed25519Signature,
      })
      .strict(),
    review: z
      .object({
        receipt: reviewReceiptSchema,
        sha256,
        keyId: externalId,
        signature: ed25519Signature,
      })
      .strict(),
    issuedAt: z.string().datetime({ offset: true }),
  })
  .strict()
  .superRefine((manifest, context) => {
    if (manifest.repository.baseBranch === manifest.repository.headBranch) {
      context.addIssue({
        code: 'custom',
        path: ['repository', 'headBranch'],
        message: 'Head and base branches must differ',
      });
    }
    if (new Set(manifest.requiredChecks).size !== manifest.requiredChecks.length) {
      context.addIssue({
        code: 'custom',
        path: ['requiredChecks'],
        message: 'Required checks must be unique',
      });
    }
  });

export const signedProposalSchema = z
  .object({
    keyId: externalId,
    manifest: upgradeManifestSchema,
    manifestSha256: sha256,
    signature: ed25519Signature,
  })
  .strict();

export type BenchmarkReceipt = z.infer<typeof benchmarkReceiptSchema>;
export type ReviewReceipt = z.infer<typeof reviewReceiptSchema>;
export type UpgradeManifest = z.infer<typeof upgradeManifestSchema>;
export type SignedProposal = z.infer<typeof signedProposalSchema>;

export const upgradeStates = [
  'submitted',
  'verifying_artifact',
  'proposal_verified',
  'syncing_pr',
  'waiting_checks',
  'starting_shadow',
  'checking_shadow',
  'merging',
  'promoting',
  'verifying_promotion',
  'completed',
  'rollback_pending',
  'rolled_back',
  'failed',
  'rollback_failed',
] as const;

export type UpgradeState = (typeof upgradeStates)[number];

export interface UpgradeRecord {
  id: string;
  proposalId: string;
  idempotencyKey: string;
  requestHash: string;
  envelope: SignedProposal;
  state: UpgradeState;
  prNumber: number | null;
  prUrl: string | null;
  deploymentId: string | null;
  mergeSha: string | null;
  promotionOperationId: string | null;
  promotionHealthyAt: Date | null;
  waitStartedAt: Date | null;
  nextAttemptAt: Date | null;
  attemptCount: number;
  lastErrorCode: string | null;
  lastErrorMessage: string | null;
  leaseOwner: string | null;
  leaseExpiresAt: Date | null;
  version: number;
  createdAt: Date;
  updatedAt: Date;
}

export interface AuditReceipt {
  id: string;
  upgradeId: string;
  sequence: number;
  event: string;
  fromState: UpgradeState | null;
  toState: UpgradeState;
  details: Record<string, unknown>;
  occurredAt: Date;
  previousHash: string | null;
  hash: string;
}

export interface NewUpgrade {
  id: string;
  proposalId: string;
  idempotencyKey: string;
  requestHash: string;
  envelope: SignedProposal;
}

export interface UpgradePatch {
  state?: UpgradeState;
  prNumber?: number | null;
  prUrl?: string | null;
  deploymentId?: string | null;
  mergeSha?: string | null;
  promotionOperationId?: string | null;
  promotionHealthyAt?: Date | null;
  waitStartedAt?: Date | null;
  nextAttemptAt?: Date | null;
  attemptCount?: number;
  lastErrorCode?: string | null;
  lastErrorMessage?: string | null;
}

export interface AuditEvent {
  event: string;
  details?: Record<string, unknown>;
}

export interface UpgradeStats {
  total: number;
  byState: Partial<Record<UpgradeState, number>>;
}

export class UpdaterError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status = 422,
    readonly retryable = false,
  ) {
    super(message);
  }
}

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value))
      throw new Error('Canonical JSON does not support non-finite numbers');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJson(item === undefined ? null : item)).join(',')}]`;
  }
  if (typeof value !== 'object') throw new Error('Canonical JSON contains an unsupported value');
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object)
    .filter((key) => object[key] !== undefined)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key])}`)
    .join(',')}}`;
}

export function sha256Hex(value: string | Uint8Array): string {
  return createHash('sha256').update(value).digest('hex');
}

export function hashObject(value: unknown): string {
  return sha256Hex(canonicalJson(value));
}

export function proposalSigningPayload(keyId: string, manifestHash: string): Buffer {
  return Buffer.from(`mizuki-upgrade-v1:${keyId}:${manifestHash}`, 'utf8');
}

export function receiptSigningPayload(
  kind: 'benchmark' | 'review',
  keyId: string,
  receiptHash: string,
): Buffer {
  return Buffer.from(`mizuki-${kind}-v1:${keyId}:${receiptHash}`, 'utf8');
}

export function newUpgrade(proposal: SignedProposal, idempotencyKey: string): NewUpgrade {
  return {
    id: randomUUID(),
    proposalId: proposal.manifest.proposalId,
    idempotencyKey,
    requestHash: hashObject(proposal),
    envelope: proposal,
  };
}

export function createAuditReceipt(
  upgradeId: string,
  sequence: number,
  fromState: UpgradeState | null,
  toState: UpgradeState,
  event: AuditEvent,
  occurredAt: Date,
  previousHash: string | null,
): AuditReceipt {
  const body = {
    upgradeId,
    sequence,
    event: event.event,
    fromState,
    toState,
    details: event.details ?? {},
    occurredAt: occurredAt.toISOString(),
    previousHash,
  };
  return {
    id: randomUUID(),
    ...body,
    occurredAt,
    hash: hashObject(body),
  };
}

export function publicUpgrade(record: UpgradeRecord): Record<string, unknown> {
  const manifest = record.envelope.manifest;
  return {
    id: record.id,
    proposalId: record.proposalId,
    sourceHandoffSha256: manifest.sourceHandoffSha256,
    manifestSha256: record.envelope.manifestSha256,
    artifactSha256: manifest.artifact.sha256,
    repository: manifest.repository,
    candidateSha: manifest.candidateSha,
    attestations: {
      proposal: {
        keyId: record.envelope.keyId,
        sha256: record.envelope.manifestSha256,
      },
      benchmark: {
        receiptId: manifest.benchmark.receipt.receiptId,
        keyId: manifest.benchmark.keyId,
        sha256: manifest.benchmark.sha256,
      },
      review: {
        receiptId: manifest.review.receipt.receiptId,
        keyId: manifest.review.keyId,
        sha256: manifest.review.sha256,
      },
    },
    state: record.state,
    prNumber: record.prNumber,
    prUrl: record.prUrl,
    deploymentId: record.deploymentId,
    mergeSha: record.mergeSha,
    promotionOperationId: record.promotionOperationId,
    promotionHealthyAt: record.promotionHealthyAt?.toISOString() ?? null,
    nextAttemptAt: record.nextAttemptAt?.toISOString() ?? null,
    lastError:
      record.lastErrorCode === null
        ? null
        : { code: record.lastErrorCode, message: record.lastErrorMessage },
    createdAt: record.createdAt.toISOString(),
    updatedAt: record.updatedAt.toISOString(),
  };
}

export function publicAudit(receipt: AuditReceipt): Record<string, unknown> {
  return {
    id: receipt.id,
    sequence: receipt.sequence,
    event: receipt.event,
    fromState: receipt.fromState,
    toState: receipt.toState,
    details: receipt.details,
    occurredAt: receipt.occurredAt.toISOString(),
    previousHash: receipt.previousHash,
    hash: receipt.hash,
  };
}

const allowedTransitions: Record<UpgradeState, ReadonlySet<UpgradeState>> = {
  submitted: new Set(['submitted', 'verifying_artifact', 'failed']),
  verifying_artifact: new Set(['verifying_artifact', 'proposal_verified', 'failed']),
  proposal_verified: new Set(['proposal_verified', 'syncing_pr', 'failed']),
  syncing_pr: new Set(['syncing_pr', 'waiting_checks', 'failed']),
  waiting_checks: new Set(['waiting_checks', 'starting_shadow', 'failed']),
  starting_shadow: new Set(['starting_shadow', 'checking_shadow', 'failed']),
  checking_shadow: new Set(['checking_shadow', 'merging', 'rollback_pending']),
  merging: new Set(['merging', 'promoting', 'rollback_pending']),
  promoting: new Set(['promoting', 'verifying_promotion', 'rollback_pending']),
  verifying_promotion: new Set(['verifying_promotion', 'completed', 'rollback_pending']),
  completed: new Set(['completed']),
  rollback_pending: new Set(['rollback_pending', 'rolled_back', 'rollback_failed']),
  rolled_back: new Set(['rolled_back']),
  failed: new Set(['failed']),
  rollback_failed: new Set(['rollback_failed']),
};

export function assertStateTransition(from: UpgradeState, to: UpgradeState): void {
  if (!allowedTransitions[from].has(to)) {
    throw new UpdaterError(
      'invalid_state_transition',
      `Upgrade cannot transition from ${from} to ${to}`,
      409,
    );
  }
}
