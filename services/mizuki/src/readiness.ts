import { z } from 'zod';
import type { PolicyReadiness } from './policy-client.js';

export const serviceDependencies = [
  'postgres',
  'operator_controls',
  'coding_gateway',
  'policy_signer',
  'github_app',
  'reviewer_route',
  'updater',
  'x402_facilitator',
] as const;

export type ServiceDependency = (typeof serviceDependencies)[number];
export type ReadinessProbe = () => Promise<unknown>;

const atomicSchema = z
  .string()
  .regex(/^[0-9]+$/)
  .nullable();
const refundProtectionSchema = z
  .object({
    refundTreasury: z.string().min(1),
    refundMint: z.string().min(1),
    refundDecimals: z.number().int().min(0).max(18),
    finalizedBalanceRaw: atomicSchema,
    pendingRefundRaw: atomicSchema,
    treasuryAvailableRefundRaw: atomicSchema,
    remainingRefundLimitUsdCents: z.number().int().nonnegative().nullable(),
    availableRefundRaw: atomicSchema,
    escrowAuthority: z.string().min(1),
    finalizedEscrowBalanceLamports: atomicSchema,
    availableEscrowReserveLamports: atomicSchema,
  })
  .strict();

export type RefundProtectionEvidence = z.infer<typeof refundProtectionSchema>;

export interface DependencyEvidence {
  ok: boolean;
  checkedAt: string;
  latencyMs: number;
  refundProtection?: RefundProtectionEvidence;
}

export interface ServiceReadinessReport {
  ready: boolean;
  checkedAt: string;
  ageMs: number;
  lastSuccessfulAt: string | null;
  lastSuccessfulAgeMs: number | null;
  dependencies: Record<ServiceDependency, DependencyEvidence>;
  failed: Array<ServiceDependency | 'stale'>;
}

interface Options {
  refreshMs: number;
  maxAgeMs: number;
  timeoutMs: number;
  failureRetryMs?: number;
  now?: () => number;
}

interface Snapshot {
  checkedAtMs: number;
  lastSuccessfulAtMs?: number;
  dependencies: Record<ServiceDependency, DependencyEvidence>;
}

export class ServiceReadiness {
  private readonly now: () => number;
  private readonly failureRetryMs: number;
  private snapshot?: Snapshot;
  private inFlight?: Promise<Snapshot>;

  constructor(
    private readonly probes: Record<ServiceDependency, ReadinessProbe>,
    private readonly options: Options,
  ) {
    if (options.refreshMs <= 0 || options.maxAgeMs < options.refreshMs) {
      throw new Error('readiness freshness bounds are invalid');
    }
    if (options.timeoutMs <= 0 || options.timeoutMs > options.maxAgeMs) {
      throw new Error('readiness timeout must be positive and no greater than max age');
    }
    this.now = options.now ?? Date.now;
    this.failureRetryMs = options.failureRetryMs ?? Math.min(5_000, options.refreshMs);
  }

  async check(): Promise<ServiceReadinessReport> {
    const cached = this.snapshot;
    if (cached) {
      const age = this.age(cached);
      const ready = serviceDependencies.every((name) => cached.dependencies[name].ok);
      const ttl = ready ? this.options.refreshMs : this.failureRetryMs;
      if (age <= ttl) return this.report(cached);
    }

    this.inFlight ??= this.refresh().finally(() => {
      this.inFlight = undefined;
    });
    return this.report(await this.inFlight);
  }

  latest(): ServiceReadinessReport | undefined {
    return this.snapshot ? this.report(this.snapshot) : undefined;
  }

  private async refresh(): Promise<Snapshot> {
    const entries = await Promise.all(
      serviceDependencies.map(async (name) => [name, await this.probe(name)] as const),
    );
    const dependencies = Object.fromEntries(entries) as Record<
      ServiceDependency,
      DependencyEvidence
    >;
    const checkedAtMs = this.now();
    const snapshot = {
      checkedAtMs,
      lastSuccessfulAtMs: serviceDependencies.every((name) => dependencies[name].ok)
        ? checkedAtMs
        : this.snapshot?.lastSuccessfulAtMs,
      dependencies,
    };
    this.snapshot = snapshot;
    return snapshot;
  }

  private async probe(name: ServiceDependency): Promise<DependencyEvidence> {
    const startedAt = this.now();
    let ok = false;
    let refundProtection: RefundProtectionEvidence | undefined;
    try {
      const result = await withTimeout(this.probes[name](), this.options.timeoutMs);
      if (name === 'policy_signer') refundProtection = refundProtectionSchema.parse(result);
      ok = true;
    } catch {
      ok = false;
    }
    const finishedAt = this.now();
    return {
      ok,
      checkedAt: new Date(finishedAt).toISOString(),
      latencyMs: Math.max(0, finishedAt - startedAt),
      ...(refundProtection ? { refundProtection } : {}),
    };
  }

  private report(snapshot: Snapshot): ServiceReadinessReport {
    const failed: Array<ServiceDependency | 'stale'> = serviceDependencies.filter(
      (name) => !snapshot.dependencies[name].ok,
    );
    const lastSuccessfulAgeMs =
      snapshot.lastSuccessfulAtMs === undefined
        ? null
        : Math.max(0, this.now() - snapshot.lastSuccessfulAtMs);
    if (lastSuccessfulAgeMs === null || lastSuccessfulAgeMs > this.options.maxAgeMs) {
      failed.push('stale');
    }
    return {
      ready: failed.length === 0,
      checkedAt: new Date(snapshot.checkedAtMs).toISOString(),
      ageMs: this.age(snapshot),
      lastSuccessfulAt:
        snapshot.lastSuccessfulAtMs === undefined
          ? null
          : new Date(snapshot.lastSuccessfulAtMs).toISOString(),
      lastSuccessfulAgeMs,
      dependencies: snapshot.dependencies,
      failed,
    };
  }

  private age(snapshot: Snapshot): number {
    return Math.max(0, this.now() - snapshot.checkedAtMs);
  }
}

export function refundProtectionEvidence(readiness: PolicyReadiness): RefundProtectionEvidence {
  return refundProtectionSchema.parse({
    refundTreasury: readiness.refundTreasury,
    refundMint: readiness.refundMint,
    refundDecimals: readiness.refundDecimals,
    finalizedBalanceRaw: readiness.finalizedBalanceRaw,
    pendingRefundRaw: readiness.pendingRefundRaw,
    treasuryAvailableRefundRaw: readiness.treasuryAvailableRefundRaw,
    remainingRefundLimitUsdCents: readiness.remainingRefundLimitUsdCents,
    availableRefundRaw: readiness.availableRefundRaw,
    escrowAuthority: readiness.escrowAuthority,
    finalizedEscrowBalanceLamports: readiness.finalizedEscrowBalanceLamports,
    availableEscrowReserveLamports: readiness.availableEscrowReserveLamports,
  });
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('readiness probe timed out')), timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}
