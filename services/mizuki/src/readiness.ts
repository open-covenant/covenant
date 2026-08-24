import { z } from 'zod';
import type { PolicyReadiness } from './policy-client.js';

export const serviceDependencies = [
  'configuration',
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
export type ApplicationDependency = Exclude<ServiceDependency, 'updater'>;
export type ReadinessProbe = () => Promise<unknown>;

export const applicationDependencies: readonly ApplicationDependency[] = serviceDependencies.filter(
  (name): name is ApplicationDependency => name !== 'updater',
);

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
const configurationSchema = z.object({ issues: z.array(z.string().min(1)) }).strict();

export type RefundProtectionEvidence = z.infer<typeof refundProtectionSchema>;

export interface DependencyEvidence {
  ok: boolean;
  checkedAt: string;
  latencyMs: number;
  configurationIssues?: string[];
  refundProtection?: RefundProtectionEvidence;
}

interface ReadinessReport<Dependency extends ServiceDependency> {
  ready: boolean;
  checkedAt: string;
  ageMs: number;
  lastSuccessfulAt: string | null;
  lastSuccessfulAgeMs: number | null;
  dependencies: Record<Dependency, DependencyEvidence>;
  failed: Array<Dependency | 'stale'>;
}

export type ServiceReadinessReport = ReadinessReport<ServiceDependency>;
export type ApplicationReadinessReport = ReadinessReport<ApplicationDependency>;

interface Options {
  refreshMs: number;
  maxAgeMs: number;
  timeoutMs: number;
  failureRetryMs?: number;
  now?: () => number;
}

interface Snapshot<Dependency extends ServiceDependency> {
  checkedAtMs: number;
  lastSuccessfulAtMs?: number;
  dependencies: Record<Dependency, DependencyEvidence>;
}

interface ScopeState<Dependency extends ServiceDependency> {
  snapshot?: Snapshot<Dependency>;
  inFlight?: Promise<Snapshot<Dependency>>;
}

export class ServiceReadiness {
  private readonly now: () => number;
  private readonly failureRetryMs: number;
  private readonly operatorState: ScopeState<ServiceDependency> = {};
  private readonly applicationState: ScopeState<ApplicationDependency> = {};

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
    return this.checkScope(serviceDependencies, this.operatorState);
  }

  async checkApplication(): Promise<ApplicationReadinessReport> {
    return this.checkScope(applicationDependencies, this.applicationState);
  }

  latest(): ServiceReadinessReport | undefined {
    const snapshot = this.operatorState.snapshot;
    return snapshot ? this.report(snapshot, serviceDependencies) : undefined;
  }

  private async checkScope<Dependency extends ServiceDependency>(
    dependencies: readonly Dependency[],
    state: ScopeState<Dependency>,
  ): Promise<ReadinessReport<Dependency>> {
    const cached = state.snapshot;
    if (cached) {
      const age = this.age(cached);
      const ready = dependencies.every((name) => cached.dependencies[name].ok);
      const ttl = ready ? this.options.refreshMs : this.failureRetryMs;
      if (age <= ttl) return this.report(cached, dependencies);
    }

    state.inFlight ??= this.refresh(dependencies, state).finally(() => {
      state.inFlight = undefined;
    });
    return this.report(await state.inFlight, dependencies);
  }

  private async refresh<Dependency extends ServiceDependency>(
    scope: readonly Dependency[],
    state: ScopeState<Dependency>,
  ): Promise<Snapshot<Dependency>> {
    const entries = await Promise.all(
      scope.map(async (name) => [name, await this.probe(name)] as const),
    );
    const dependencies = Object.fromEntries(entries) as Record<Dependency, DependencyEvidence>;
    const checkedAtMs = this.now();
    const snapshot = {
      checkedAtMs,
      lastSuccessfulAtMs: scope.every((name) => dependencies[name].ok)
        ? checkedAtMs
        : state.snapshot?.lastSuccessfulAtMs,
      dependencies,
    };
    state.snapshot = snapshot;
    return snapshot;
  }

  private async probe(name: ServiceDependency): Promise<DependencyEvidence> {
    const startedAt = this.now();
    let ok = false;
    let configurationIssues: string[] | undefined;
    let refundProtection: RefundProtectionEvidence | undefined;
    try {
      const result = await withTimeout(this.probes[name](), this.options.timeoutMs);
      if (name === 'configuration') {
        configurationIssues = configurationSchema.parse(result).issues;
        if (configurationIssues.length > 0) throw new Error('configuration is incomplete');
      }
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
      ...(configurationIssues ? { configurationIssues } : {}),
      ...(refundProtection ? { refundProtection } : {}),
    };
  }

  private report<Dependency extends ServiceDependency>(
    snapshot: Snapshot<Dependency>,
    dependencies: readonly Dependency[],
  ): ReadinessReport<Dependency> {
    const failed: Array<Dependency | 'stale'> = dependencies.filter(
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

  private age<Dependency extends ServiceDependency>(snapshot: Snapshot<Dependency>): number {
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
