import { createHash } from 'node:crypto';
import type { SandboxProvider } from './types.js';

export type ReadinessDependency = 'model' | 'balance' | 'sandbox' | 'tariff';

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

interface BalanceProbe {
  check: () => Promise<void>;
}

interface TariffProbe {
  check: () => Promise<{ validUntilMs: number }>;
}

interface Options {
  provider: SandboxProvider;
  model: ModelProbe;
  balance?: BalanceProbe;
  tariff?: TariffProbe;
  refreshMs: number;
  maxAgeMs: number;
  timeoutMs: number;
  failureRetryMs?: number;
  now?: () => number;
}

interface Snapshot {
  checkedAtMs: number;
  lastSuccessfulAtMs?: number;
  tariffValidUntilMs?: number;
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
      const ready = this.snapshotReady(cached);
      const ttl = ready ? this.options.refreshMs : this.failureRetryMs;
      if (age <= ttl) return this.report(cached);
    }

    this.inFlight ??= this.refresh().finally(() => {
      this.inFlight = undefined;
    });
    return this.report(await this.inFlight);
  }

  private async refresh(): Promise<Snapshot> {
    let tariffValidUntilMs: number | undefined;
    const [model, balance, sandbox, tariff] = await Promise.all([
      this.probe('model', () => this.probeModel()),
      this.probe('balance', () => this.probeBalance()),
      this.probe('sandbox', () => this.probeSandbox()),
      this.probe('tariff', async () => {
        tariffValidUntilMs = await this.probeTariff();
      }),
    ]);
    const checkedAtMs = this.now();
    const dependencies = { model, balance, sandbox, tariff };
    const snapshot = {
      checkedAtMs,
      tariffValidUntilMs,
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
    await this.options.provider.check?.();
  }

  private async probeBalance(): Promise<void> {
    await this.options.balance?.check();
  }

  private async probeTariff(): Promise<number | undefined> {
    return (await this.options.tariff?.check())?.validUntilMs;
  }

  private report(snapshot: Snapshot): GatewayReadinessReport {
    const ageMs = this.age(snapshot);
    const failed: Array<ReadinessDependency | 'stale'> = (
      Object.entries(snapshot.dependencies) as Array<[ReadinessDependency, ReadinessEvidence]>
    )
      .filter(([, evidence]) => !evidence.ok)
      .map(([dependency]) => dependency);
    if (
      snapshot.tariffValidUntilMs !== undefined &&
      snapshot.tariffValidUntilMs <= this.now() &&
      !failed.includes('tariff')
    ) {
      failed.push('tariff');
    }
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

  private snapshotReady(snapshot: Snapshot): boolean {
    return (
      dependenciesReady(snapshot.dependencies) &&
      (snapshot.tariffValidUntilMs === undefined || snapshot.tariffValidUntilMs > this.now())
    );
  }
}

function dependenciesReady(dependencies: Record<ReadinessDependency, ReadinessEvidence>): boolean {
  return (
    dependencies.model.ok &&
    dependencies.balance.ok &&
    dependencies.sandbox.ok &&
    dependencies.tariff.ok
  );
}

export interface E2bTariffExpectation {
  reference: string;
  templateId: string;
  cpuCount: number;
  memoryMb: number;
  worstCaseUsdPerSec: number;
}

interface E2bTariffEvidence {
  schema: 'mizuki.e2b-tariff.v1';
  provider: 'e2b';
  effectiveAt: string;
  validUntil: string;
  sourceUrl: string;
  sourceSha256: string;
  templateId: string;
  cpuCount: number;
  memoryMb: number;
  cpuUsdPerCoreSecond: number;
  memoryUsdPerGibSecond: number;
  fixedUsdPerSecond: number;
  safetyMultiplier: number;
  worstCaseUsdPerSecond: number;
}

const MAX_TARIFF_BYTES = 64 * 1024;
const MAX_TARIFF_VALIDITY_MS = 7 * 24 * 60 * 60 * 1_000;
const MAX_FUTURE_SKEW_MS = 5 * 60 * 1_000;

export async function verifyE2bTariff(
  expected: E2bTariffExpectation,
  fetcher: typeof fetch = fetch,
  now: () => number = Date.now,
): Promise<{ validUntilMs: number }> {
  const reference = new URL(expected.reference);
  const digest = reference.hash.match(/^#sha256=([a-f0-9]{64})$/i)?.[1]?.toLowerCase();
  if (reference.protocol !== 'https:' || !digest) {
    throw new Error('sandbox tariff reference is not content-addressed HTTPS');
  }
  reference.hash = '';
  const raw = await fetchBounded(fetcher, reference, MAX_TARIFF_BYTES, 'tariff evidence');
  const actualDigest = createHash('sha256').update(raw).digest('hex');
  if (actualDigest !== digest) throw new Error('sandbox tariff evidence digest mismatch');

  let document: unknown;
  try {
    document = JSON.parse(raw.toString('utf8'));
  } catch {
    throw new Error('sandbox tariff evidence is not valid JSON');
  }
  if (!isE2bTariffEvidence(document)) {
    throw new Error('sandbox tariff evidence schema is invalid');
  }
  if (
    document.templateId !== expected.templateId ||
    document.cpuCount !== expected.cpuCount ||
    document.memoryMb !== expected.memoryMb ||
    Math.abs(document.worstCaseUsdPerSecond - expected.worstCaseUsdPerSec) > 1e-12
  ) {
    throw new Error('sandbox tariff evidence does not match the configured sandbox identity');
  }
  const source = new URL(document.sourceUrl);
  if (
    source.protocol !== 'https:' ||
    (!source.hostname.endsWith('.e2b.dev') &&
      source.hostname !== 'e2b.dev' &&
      !source.hostname.endsWith('.e2b.ai') &&
      source.hostname !== 'e2b.ai')
  ) {
    throw new Error('sandbox tariff evidence source is not an official E2B HTTPS origin');
  }
  const effectiveAt = Date.parse(document.effectiveAt);
  const validUntil = Date.parse(document.validUntil);
  const checkedAt = now();
  if (!Number.isFinite(effectiveAt) || !Number.isFinite(validUntil)) {
    throw new Error('sandbox tariff evidence validity window is invalid');
  }
  if (
    effectiveAt > checkedAt + MAX_FUTURE_SKEW_MS ||
    effectiveAt < checkedAt - MAX_TARIFF_VALIDITY_MS ||
    validUntil <= checkedAt ||
    validUntil <= effectiveAt ||
    validUntil - effectiveAt > MAX_TARIFF_VALIDITY_MS
  ) {
    throw new Error('sandbox tariff evidence is stale, future-dated, or valid for too long');
  }

  const baseRate =
    document.fixedUsdPerSecond +
    document.cpuCount * document.cpuUsdPerCoreSecond +
    (document.memoryMb / 1024) * document.memoryUsdPerGibSecond;
  const requiredRate = baseRate * document.safetyMultiplier;
  if (!Number.isFinite(requiredRate) || expected.worstCaseUsdPerSec + 1e-12 < requiredRate) {
    throw new Error(
      'configured sandbox worst-case tariff does not cover the verified rate formula',
    );
  }
  return { validUntilMs: validUntil };
}

function isE2bTariffEvidence(value: unknown): value is E2bTariffEvidence {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const document = value as Partial<E2bTariffEvidence>;
  return (
    document.schema === 'mizuki.e2b-tariff.v1' &&
    document.provider === 'e2b' &&
    typeof document.effectiveAt === 'string' &&
    typeof document.validUntil === 'string' &&
    typeof document.sourceUrl === 'string' &&
    typeof document.sourceSha256 === 'string' &&
    /^[a-f0-9]{64}$/i.test(document.sourceSha256) &&
    typeof document.templateId === 'string' &&
    positiveInteger(document.cpuCount) &&
    positiveInteger(document.memoryMb) &&
    nonNegativeFinite(document.cpuUsdPerCoreSecond) &&
    nonNegativeFinite(document.memoryUsdPerGibSecond) &&
    nonNegativeFinite(document.fixedUsdPerSecond) &&
    typeof document.safetyMultiplier === 'number' &&
    Number.isFinite(document.safetyMultiplier) &&
    document.safetyMultiplier >= 1 &&
    document.safetyMultiplier <= 100 &&
    typeof document.worstCaseUsdPerSecond === 'number' &&
    Number.isFinite(document.worstCaseUsdPerSecond) &&
    document.worstCaseUsdPerSecond > 0 &&
    document.worstCaseUsdPerSecond <= 0.01 &&
    (document.cpuUsdPerCoreSecond > 0 || document.memoryUsdPerGibSecond > 0)
  );
}

async function fetchBounded(
  fetcher: typeof fetch,
  url: URL,
  maxBytes: number,
  label: string,
): Promise<Buffer> {
  const response = await fetcher(url, {
    cache: 'no-store',
    redirect: 'follow',
    signal: AbortSignal.timeout(15_000),
  });
  if (!response.ok) throw new Error(`sandbox ${label} failed with HTTP ${response.status}`);
  const declaredLength = Number(response.headers.get('content-length'));
  if (Number.isFinite(declaredLength) && declaredLength > maxBytes) {
    throw new Error(`sandbox ${label} exceeds the size limit`);
  }
  const raw = Buffer.from(await response.arrayBuffer());
  if (raw.byteLength > maxBytes) throw new Error(`sandbox ${label} exceeds the size limit`);
  return raw;
}

function positiveInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value > 0;
}

function nonNegativeFinite(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
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
