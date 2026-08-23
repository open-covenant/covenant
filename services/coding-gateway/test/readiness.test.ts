import { createHash } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { GatewayReadiness, verifyE2bTariff } from '../src/readiness.js';
import type { Sandbox, SandboxProvider } from '../src/types.js';

describe('GatewayReadiness', () => {
  it('runs and caches non-billable provider control-plane evidence', async () => {
    let now = 1_000;
    const modelCheck = vi.fn(async () => undefined);
    const sandbox = fakeSandbox();
    const provider = fakeProvider(sandbox);
    const readiness = createReadiness(provider, modelCheck, () => now);

    const first = await readiness.check();
    expect(first).toMatchObject({
      ready: true,
      model: 'deepseek-v3.2',
      checkedAt: new Date(now).toISOString(),
      ageMs: 0,
      lastSuccessfulAgeMs: 0,
      failed: [],
      dependencies: { model: { ok: true }, sandbox: { ok: true } },
    });
    await readiness.check();
    expect(modelCheck).toHaveBeenCalledTimes(1);
    expect(provider.check).toHaveBeenCalledTimes(1);
    expect(provider.create).not.toHaveBeenCalled();

    now += 101;
    await readiness.check();
    expect(modelCheck).toHaveBeenCalledTimes(2);
    expect(provider.check).toHaveBeenCalledTimes(2);
    expect(provider.create).not.toHaveBeenCalled();
  });

  it('coalesces concurrent refreshes', async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const modelCheck = vi.fn(async () => {
      await gate;
    });
    const provider = fakeProvider(fakeSandbox());
    const readiness = createReadiness(provider, modelCheck, () => 1_000);

    const checks = [readiness.check(), readiness.check(), readiness.check()];
    release();
    await expect(Promise.all(checks)).resolves.toHaveLength(3);
    expect(modelCheck).toHaveBeenCalledTimes(1);
    expect(provider.check).toHaveBeenCalledTimes(1);
    expect(provider.create).not.toHaveBeenCalled();
  });

  it('fails closed when evidence is stale or a dependency fails, then recovers', async () => {
    let now = 1_000;
    let modelHealthy = true;
    const modelCheck = vi.fn(async () => {
      if (!modelHealthy) throw new Error('model unavailable');
    });
    const provider = fakeProvider(fakeSandbox());
    const readiness = createReadiness(provider, modelCheck, () => now);

    await expect(readiness.check()).resolves.toMatchObject({ ready: true });
    modelHealthy = false;
    now += 301;
    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      lastSuccessfulAgeMs: 301,
      failed: ['model', 'stale'],
    });

    modelHealthy = true;
    now += 11;
    await expect(readiness.check()).resolves.toMatchObject({
      ready: true,
      lastSuccessfulAgeMs: 0,
      failed: [],
    });
  });

  it('requires successful model and sandbox control-plane evidence', async () => {
    const failedModel = vi.fn(async () => {
      throw new Error('model evidence is invalid');
    });
    const badProvider = fakeProvider(fakeSandbox());
    badProvider.check.mockRejectedValueOnce(new Error('sandbox control plane failed'));
    const readiness = createReadiness(badProvider, failedModel, () => 1_000);

    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      failed: expect.arrayContaining(['model', 'sandbox', 'stale']),
    });
  });

  it('fails closed when funded-balance evidence is below the configured floor', async () => {
    const readiness = new GatewayReadiness({
      provider: fakeProvider(fakeSandbox()),
      model: { expectedModel: 'deepseek-v3.2', check: vi.fn(async () => undefined) },
      balance: {
        check: vi.fn(async () => {
          throw new Error('balance below floor');
        }),
      },
      refreshMs: 100,
      maxAgeMs: 300,
      timeoutMs: 20,
      failureRetryMs: 10,
      now: () => 1_000,
    });

    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      dependencies: { balance: { ok: false } },
      failed: expect.arrayContaining(['balance', 'stale']),
    });
  });

  it('does not serve cached readiness past the tariff validity deadline', async () => {
    let now = 1_000;
    const tariffCheck = vi.fn(async () => {
      if (now >= 1_050) throw new Error('tariff expired');
      return { validUntilMs: 1_050 };
    });
    const readiness = new GatewayReadiness({
      provider: fakeProvider(fakeSandbox()),
      model: { expectedModel: 'deepseek-v3.2', check: vi.fn(async () => undefined) },
      tariff: { check: tariffCheck },
      refreshMs: 100,
      maxAgeMs: 300,
      timeoutMs: 20,
      failureRetryMs: 10,
      now: () => now,
    });

    await expect(readiness.check()).resolves.toMatchObject({ ready: true });
    now = 1_051;
    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      failed: expect.arrayContaining(['tariff']),
    });
    expect(tariffCheck).toHaveBeenCalledTimes(2);
  });
});

describe('E2B tariff readiness', () => {
  const source = 'official provider rate card';
  const expected = {
    templateId: 'tpl_immutable_123',
    cpuCount: 4,
    memoryMb: 4096,
    worstCaseUsdPerSec: 0.0002,
  };
  const evidence = {
    schema: 'mizuki.e2b-tariff.v1',
    provider: 'e2b',
    effectiveAt: '2026-08-23T00:00:00.000Z',
    validUntil: '2026-08-30T00:00:00.000Z',
    sourceUrl: 'https://e2b.dev/pricing',
    sourceSha256: createHash('sha256').update(source).digest('hex'),
    templateId: expected.templateId,
    cpuCount: expected.cpuCount,
    memoryMb: expected.memoryMb,
    worstCaseUsdPerSecond: expected.worstCaseUsdPerSec,
    cpuUsdPerCoreSecond: 0.000014,
    memoryUsdPerGibSecond: 0.0000045,
    fixedUsdPerSecond: 0,
    safetyMultiplier: 2,
  };

  it('fetches and hashes evidence whose formula covers the verified template resources', async () => {
    const raw = JSON.stringify(evidence);
    const reference = contentAddressedReference(raw);

    await expect(
      verifyE2bTariff({ reference, ...expected }, responseFetcher(raw, source), tariffNow),
    ).resolves.toEqual({ validUntilMs: Date.parse(evidence.validUntil) });
  });

  it('fails closed on an unfetched digest, resource drift, or an understated formula', async () => {
    const raw = JSON.stringify(evidence);
    await expect(
      verifyE2bTariff(
        {
          reference: 'https://evidence.example/e2b-tariff.json#sha256=' + '0'.repeat(64),
          ...expected,
        },
        responseFetcher(raw, source),
        tariffNow,
      ),
    ).rejects.toThrow(/digest mismatch/);

    await expect(
      verifyE2bTariff(
        { reference: contentAddressedReference(raw), ...expected },
        responseFetcher(raw, 'changed provider rate card'),
        tariffNow,
      ),
    ).rejects.toThrow(/source digest mismatch/);

    const drifted = JSON.stringify({ ...evidence, cpuCount: 8 });
    await expect(
      verifyE2bTariff(
        { reference: contentAddressedReference(drifted), ...expected },
        responseFetcher(drifted, source),
        tariffNow,
      ),
    ).rejects.toThrow(/does not match/);

    const understated = JSON.stringify({ ...evidence, safetyMultiplier: 3 });
    await expect(
      verifyE2bTariff(
        { reference: contentAddressedReference(understated), ...expected },
        responseFetcher(understated, source),
        tariffNow,
      ),
    ).rejects.toThrow(/does not cover/);

    const stale = JSON.stringify({
      ...evidence,
      effectiveAt: '2026-08-01T00:00:00.000Z',
      validUntil: '2026-08-08T00:00:00.000Z',
    });
    await expect(
      verifyE2bTariff(
        { reference: contentAddressedReference(stale), ...expected },
        responseFetcher(stale, source),
        tariffNow,
      ),
    ).rejects.toThrow(/stale/);
  });
});

function createReadiness(
  provider: SandboxProvider,
  modelCheck: () => Promise<void>,
  now: () => number,
): GatewayReadiness {
  return new GatewayReadiness({
    provider,
    now,
    model: {
      expectedModel: 'deepseek-v3.2',
      check: modelCheck,
    },
    refreshMs: 100,
    maxAgeMs: 300,
    timeoutMs: 20,
    failureRetryMs: 10,
  });
}

function fakeProvider(sandbox: Sandbox): SandboxProvider & {
  check: ReturnType<typeof vi.fn>;
  create: ReturnType<typeof vi.fn>;
} {
  return {
    id: 'e2b',
    check: vi.fn(async () => undefined),
    create: vi.fn(async () => sandbox),
  };
}

function fakeSandbox() {
  return {
    readFile: vi.fn(async () => ''),
    writeFile: vi.fn(async () => undefined),
    exec: vi.fn(async () => ({ stdout: 'mizuki-ready', stderr: '', exitCode: 0 })),
    previewUrl: vi.fn(async () => 'https://preview.test'),
    destroy: vi.fn(async () => undefined),
  };
}

function contentAddressedReference(raw: string): string {
  const digest = createHash('sha256').update(raw).digest('hex');
  return `https://evidence.example/e2b-tariff.json#sha256=${digest}`;
}

function responseFetcher(evidence: string, source: string): typeof fetch {
  return vi.fn(async (input: URL | RequestInfo) => {
    const url = input instanceof URL ? input : new URL(String(input));
    return new Response(url.hostname.endsWith('e2b.dev') ? source : evidence, { status: 200 });
  }) as unknown as typeof fetch;
}

const tariffNow = () => Date.parse('2026-08-24T00:00:00.000Z');
