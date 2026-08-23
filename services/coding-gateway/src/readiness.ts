import type { SandboxProvider } from './types.js';

export type ReadinessDependency = 'model' | 'sandbox';

export interface ReadinessEvidence {
  ok: boolean;
  checkedAt: string;
  latencyMs: number;
}

export interface GatewayReadinessReport {
  ready: boolean;
  model: string;
  checkedAt: string;
  ageMs: number;
  lastSuccessfulAt: string | null;
  lastSuccessfulAgeMs: number | null;
  dependencies: Record<ReadinessDependency, ReadinessEvidence>;
  failed: Array<ReadinessDependency | 'stale'>;
}

interface ModelProbe {
  expectedModel: string;
  check: () => Promise<void>;
}

interface Options {
  provider: SandboxProvider;
  model: ModelProbe;
  refreshMs: number;
  maxAgeMs: number;
  timeoutMs: number;
  failureRetryMs?: number;
  now?: () => number;
}

interface Snapshot {
  checkedAtMs: number;
  lastSuccessfulAtMs?: number;
  dependencies: Record<ReadinessDependency, ReadinessEvidence>;
}

export class GatewayReadiness {
  private readonly now: () => number;
  private readonly failureRetryMs: number;
  private snapshot?: Snapshot;
  private inFlight?: Promise<Snapshot>;

  constructor(private readonly options: Options) {
    if (options.refreshMs <= 0 || options.maxAgeMs < options.refreshMs) {
      throw new Error('readiness freshness bounds are invalid');
    }
    if (options.timeoutMs <= 0 || options.timeoutMs > options.maxAgeMs) {
      throw new Error('readiness timeout must be positive and no greater than max age');
    }
    this.now = options.now ?? Date.now;
    this.failureRetryMs = options.failureRetryMs ?? Math.min(5_000, options.refreshMs);
  }

  async check(): Promise<GatewayReadinessReport> {
    const cached = this.snapshot;
    if (cached) {
      const age = this.age(cached);
      const ready = dependenciesReady(cached.dependencies);
      const ttl = ready ? this.options.refreshMs : this.failureRetryMs;
      if (age <= ttl) return this.report(cached);
    }

    this.inFlight ??= this.refresh().finally(() => {
      this.inFlight = undefined;
    });
    return this.report(await this.inFlight);
  }

  private async refresh(): Promise<Snapshot> {
    const [model, sandbox] = await Promise.all([
      this.probe('model', () => this.probeModel()),
      this.probe('sandbox', () => this.probeSandbox()),
    ]);
    const checkedAtMs = this.now();
    const dependencies = { model, sandbox };
    const snapshot = {
      checkedAtMs,
      lastSuccessfulAtMs: dependenciesReady(dependencies)
        ? checkedAtMs
        : this.snapshot?.lastSuccessfulAtMs,
      dependencies,
    };
    this.snapshot = snapshot;
    return snapshot;
  }

  private async probe(
    dependency: ReadinessDependency,
    check: () => Promise<void>,
  ): Promise<ReadinessEvidence> {
    const startedAt = this.now();
    let ok = false;
    try {
      await withTimeout(check(), this.options.timeoutMs, `${dependency} readiness timed out`);
      ok = true;
    } catch {
      ok = false;
    }
    const finishedAt = this.now();
    return {
      ok,
      checkedAt: new Date(finishedAt).toISOString(),
      latencyMs: Math.max(0, finishedAt - startedAt),
    };
  }

  private async probeModel(): Promise<void> {
    await this.options.model.check();
  }

  private async probeSandbox(): Promise<void> {
    const sandbox = await this.options.provider.create({
      runId: `readiness-${this.now()}`,
      egressAllowlist: [],
      cpuMs: this.options.timeoutMs,
      memoryMb: 256,
      diskMb: 64,
      wallMs: this.options.timeoutMs,
    });
    let probeError: unknown;
    try {
      const result = await sandbox.exec('node -e "process.stdout.write(\'mizuki-ready\')"', {
        timeoutMs: this.options.timeoutMs,
      });
      if (result.exitCode !== 0 || result.stdout !== 'mizuki-ready') {
        throw new Error('sandbox execution evidence is invalid');
      }
    } catch (cause) {
      probeError = cause;
    }

    try {
      await withTimeout(
        sandbox.destroy(),
        this.options.timeoutMs,
        'sandbox destroy readiness timed out',
      );
    } catch (cause) {
      probeError ??= cause;
    }
    if (probeError) throw probeError;
  }

  private report(snapshot: Snapshot): GatewayReadinessReport {
    const ageMs = this.age(snapshot);
    const failed: Array<ReadinessDependency | 'stale'> = (
      Object.entries(snapshot.dependencies) as Array<[ReadinessDependency, ReadinessEvidence]>
    )
      .filter(([, evidence]) => !evidence.ok)
      .map(([dependency]) => dependency);
    const lastSuccessfulAgeMs =
      snapshot.lastSuccessfulAtMs === undefined
        ? null
        : Math.max(0, this.now() - snapshot.lastSuccessfulAtMs);
    if (lastSuccessfulAgeMs === null || lastSuccessfulAgeMs > this.options.maxAgeMs) {
      failed.push('stale');
    }
    return {
      ready: failed.length === 0,
      model: this.options.model.expectedModel,
      checkedAt: new Date(snapshot.checkedAtMs).toISOString(),
      ageMs,
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

function dependenciesReady(dependencies: Record<ReadinessDependency, ReadinessEvidence>): boolean {
  return dependencies.model.ok && dependencies.sandbox.ok;
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}
