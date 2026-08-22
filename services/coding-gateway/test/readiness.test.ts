import { describe, expect, it, vi } from 'vitest';
import { GatewayReadiness } from '../src/readiness.js';
import type { Sandbox, SandboxProvider } from '../src/types.js';

describe('GatewayReadiness', () => {
  it('authenticates the model catalog and caches fresh create/exec/destroy evidence', async () => {
    let now = 1_000;
    const request = vi.fn<typeof fetch>(async (_url, init) => {
      expect(init?.headers).toMatchObject({ authorization: 'Bearer secret' });
      return Response.json({ data: [{ id: 'deepseek-v3.2' }] });
    });
    const sandbox = fakeSandbox();
    const provider = fakeProvider(sandbox);
    const readiness = createReadiness(provider, request, () => now);

    const first = await readiness.check();
    expect(first).toMatchObject({
      ready: true,
      checkedAt: new Date(now).toISOString(),
      ageMs: 0,
      lastSuccessfulAgeMs: 0,
      failed: [],
      dependencies: { model: { ok: true }, sandbox: { ok: true } },
    });
    await readiness.check();
    expect(request).toHaveBeenCalledTimes(1);
    expect(provider.create).toHaveBeenCalledTimes(1);

    now += 101;
    await readiness.check();
    expect(request).toHaveBeenCalledTimes(2);
    expect(provider.create).toHaveBeenCalledTimes(2);
    expect(sandbox.destroy).toHaveBeenCalledTimes(2);
  });

  it('coalesces concurrent refreshes', async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const request = vi.fn<typeof fetch>(async () => {
      await gate;
      return Response.json({ data: [{ id: 'deepseek-v3.2' }] });
    });
    const provider = fakeProvider(fakeSandbox());
    const readiness = createReadiness(provider, request, () => 1_000);

    const checks = [readiness.check(), readiness.check(), readiness.check()];
    release();
    await expect(Promise.all(checks)).resolves.toHaveLength(3);
    expect(request).toHaveBeenCalledTimes(1);
    expect(provider.create).toHaveBeenCalledTimes(1);
  });

  it('fails closed when evidence is stale or a dependency fails, then recovers', async () => {
    let now = 1_000;
    let modelHealthy = true;
    const request = vi.fn<typeof fetch>(async () =>
      modelHealthy
        ? Response.json({ data: [{ id: 'deepseek-v3.2' }] })
        : new Response(null, { status: 503 }),
    );
    const provider = fakeProvider(fakeSandbox());
    const readiness = createReadiness(provider, request, () => now);

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

  it('requires exact model-catalog and sandbox teardown evidence', async () => {
    const malformed = vi.fn<typeof fetch>(async () => Response.json({ models: [] }));
    const badSandbox = fakeSandbox();
    badSandbox.destroy.mockRejectedValueOnce(new Error('destroy failed'));
    const readiness = createReadiness(fakeProvider(badSandbox), malformed, () => 1_000);

    await expect(readiness.check()).resolves.toMatchObject({
      ready: false,
      failed: expect.arrayContaining(['model', 'sandbox', 'stale']),
    });
  });
});

function createReadiness(
  provider: SandboxProvider,
  request: typeof fetch,
  now: () => number,
): GatewayReadiness {
  return new GatewayReadiness({
    provider,
    request,
    now,
    model: {
      url: 'https://usepod.test/v1/models',
      headers: { authorization: 'Bearer secret' },
      expectedModel: 'deepseek-v3.2',
    },
    refreshMs: 100,
    maxAgeMs: 300,
    timeoutMs: 20,
    failureRetryMs: 10,
  });
}

function fakeProvider(sandbox: Sandbox): SandboxProvider & { create: ReturnType<typeof vi.fn> } {
  return {
    id: 'e2b',
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
