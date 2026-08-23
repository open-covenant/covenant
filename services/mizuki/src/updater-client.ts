import { z } from 'zod';

export const updaterStates = [
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

export type ObservedUpdaterState = (typeof updaterStates)[number];

const sha256 = z.string().regex(/^[a-f0-9]{64}$/);
const gitSha = z.string().regex(/^[a-f0-9]{40}$/);
const externalId = z
  .string()
  .min(1)
  .max(128)
  .regex(/^[A-Za-z0-9][A-Za-z0-9._:-]*$/);
const attestationSchema = z
  .object({
    keyId: externalId,
    sha256,
  })
  .strict();
const upgradeSchema = z
  .object({
    id: z.string().uuid(),
    proposalId: externalId,
    sourceHandoffSha256: sha256,
    manifestSha256: sha256,
    artifactSha256: sha256,
    repository: z
      .object({
        owner: z.string().min(1).max(100),
        name: z.string().min(1).max(100),
        baseBranch: z.string().min(1).max(240),
        headBranch: z.string().min(1).max(240),
      })
      .strict(),
    candidateSha: gitSha,
    attestations: z
      .object({
        proposal: attestationSchema,
        benchmark: attestationSchema.extend({ receiptId: externalId }),
        review: attestationSchema.extend({ receiptId: externalId }),
      })
      .strict(),
    state: z.enum(updaterStates),
    prNumber: z.number().int().positive().nullable(),
    prUrl: z.string().url().nullable(),
    deploymentId: z.string().min(1).max(256).nullable(),
    mergeSha: gitSha.nullable(),
    promotionOperationId: externalId.nullable(),
    promotionHealthyAt: z.string().datetime({ offset: true }).nullable(),
    nextAttemptAt: z.string().datetime({ offset: true }).nullable(),
    lastError: z
      .object({ code: z.string().min(1), message: z.string().nullable() })
      .strict()
      .nullable(),
    createdAt: z.string().datetime({ offset: true }),
    updatedAt: z.string().datetime({ offset: true }),
  })
  .strict();
const responseSchema = z
  .object({
    upgrade: upgradeSchema,
    auditHeadHash: sha256,
  })
  .strict();
const readinessSchema = z
  .object({
    ready: z.literal(true),
    service: z.literal('mizuki-updater'),
    failed: z.array(z.never()).length(0),
    dependencies: z
      .object({
        postgres: z.object({ ok: z.literal(true) }).passthrough(),
        operational: z.object({ ok: z.literal(true) }).passthrough(),
      })
      .passthrough(),
  })
  .passthrough();

export type ObservedUpgrade = z.infer<typeof upgradeSchema> & { auditHeadHash: string };

export interface UpgradeStatusReader {
  getByProposalId(proposalId: string): Promise<ObservedUpgrade | undefined>;
}

export class UpdaterStatusClient implements UpgradeStatusReader {
  private readonly baseUrl: string;

  constructor(
    baseUrl: string,
    private readonly token: string,
    private readonly timeoutMs = 8_000,
    private readonly request: typeof fetch = fetch,
  ) {
    this.baseUrl = new URL(baseUrl).toString().replace(/\/$/, '');
  }

  async getByProposalId(proposalId: string): Promise<ObservedUpgrade | undefined> {
    const response = await this.request(
      `${this.baseUrl}/v1/proposals/${encodeURIComponent(proposalId)}`,
      {
        headers: {
          accept: 'application/json',
          authorization: `Bearer ${this.token}`,
        },
        signal: AbortSignal.timeout(this.timeoutMs),
      },
    );
    if (response.status === 404) return undefined;
    if (!response.ok) {
      throw new Error(`updater status request failed with HTTP ${response.status}`);
    }
    const parsed = responseSchema.parse(await response.json());
    if (parsed.upgrade.proposalId !== proposalId) {
      throw new Error('updater returned a different proposal');
    }
    return { ...parsed.upgrade, auditHeadHash: parsed.auditHeadHash };
  }

  async readiness(): Promise<void> {
    const [response] = await Promise.all([
      this.request(`${this.baseUrl}/readyz`, {
        headers: { accept: 'application/json' },
        signal: AbortSignal.timeout(this.timeoutMs),
      }),
      this.getByProposalId('mizuki-readiness-probe'),
    ]);
    if (!response.ok) throw new Error('updater service is not ready');
    readinessSchema.parse(await response.json());
  }
}
