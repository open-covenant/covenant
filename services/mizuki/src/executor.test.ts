import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { enforcePolicy, JobProcessor } from './executor.js';
import { MemoryStore } from './store.js';
import type { Job, Payment, Quote, RunArtifacts } from './types.js';

const quote: Quote = {
  id: 'quote-1',
  issueUrl: 'https://github.com/example/project/issues/1',
  owner: 'example',
  repo: 'project',
  issueNumber: 1,
  issueTitle: 'Fix docs',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: ['pnpm test'],
  expiresAt: '2099-01-01T00:00:00Z',
};
const artifacts: RunArtifacts = {
  patch: 'diff --git a/README.md b/README.md',
  changedFiles: ['README.md'],
  files: [{ path: 'README.md', content: 'fixed' }],
  validations: [{ command: 'pnpm test', exitCode: 0, stdout: '', stderr: '' }],
};

describe('enforcePolicy', () => {
  it('accepts a small validated text change', () => {
    expect(() => enforcePolicy(quote, artifacts)).not.toThrow();
  });

  it('rejects workflows and unpublishable deletes', () => {
    expect(() =>
      enforcePolicy(quote, {
        ...artifacts,
        changedFiles: ['.github/workflows/release.yml'],
        files: [{ path: '.github/workflows/release.yml', content: '' }],
      }),
    ).toThrow('forbidden path');
    expect(() => enforcePolicy(quote, { ...artifacts, files: [] })).toThrow('do not exactly match');
  });
});

describe('loadConfig', () => {
  it('uses mock mode only when explicitly requested', () => {
    expect(loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' }).paymentMode).toBe('mock');
    expect(loadConfig({}).paymentMode).toBe('live');
  });
});

describe('JobProcessor', () => {
  const payment: Payment = {
    payer: '1'.repeat(32),
    transaction: 'payment-tx',
    amountAtomic: '2000000',
  };

  it('requires an isolated, durable UsePod gateway in live mode', async () => {
    const github = { currentHead: async () => quote.baseSha, publish: async () => '' };
    const live = loadConfig({
      MIZUKI_PAYMENT_MODE: 'live',
      MIZUKI_CODING_GATEWAY_TOKEN: 'g'.repeat(32),
    });
    const request = vi.fn<typeof fetch>(async () => Response.json(gatewayReadiness()));
    const valid = new JobProcessor(live, new MemoryStore(), github, request);
    const unsafe = new JobProcessor(live, new MemoryStore(), github, async () =>
      Response.json(gatewayReadiness({ provider: 'local', persistentRuns: false })),
    );

    await expect(valid.readiness()).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'http://127.0.0.1:8642/readyz',
      expect.objectContaining({
        headers: { authorization: `Bearer ${'g'.repeat(32)}` },
      }),
    );
    await expect(unsafe.readiness()).rejects.toThrow('live isolation contract');
  });

  it('rejects malformed or stale gateway readiness evidence', async () => {
    const github = { currentHead: async () => quote.baseSha, publish: async () => '' };
    const config = loadConfig({ MIZUKI_PAYMENT_MODE: 'mock' });
    const malformed = new JobProcessor(config, new MemoryStore(), github, async () =>
      Response.json({ ready: true, backend: 'usepod', provider: 'e2b', persistentRuns: true }),
    );
    const stale = new JobProcessor(config, new MemoryStore(), github, async () =>
      Response.json(
        gatewayReadiness({
          ready: false,
          failed: ['stale'],
          lastSuccessfulAgeMs: 301_000,
        }),
      ),
    );

    await expect(malformed.readiness()).rejects.toThrow();
    await expect(stale.readiness()).rejects.toThrow('readiness evidence is invalid');
  });

  it('delivers an independently approved gateway change', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'delivery-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-1' });
      if (url.endsWith('/v1/runs/run-1')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return Response.json({
          choices: [{ message: { content: JSON.stringify({ approved: true, reason: 'scoped' }) } }],
          usage: { prompt_tokens: 50, completion_tokens: 10 },
        });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = {
      currentHead: async () => jobQuote.baseSha,
      publish: async (_job: Job) => 'https://github.com/example/project/pull/2',
    };
    const config = loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' });
    await new JobProcessor(config, store, github, request as typeof fetch).process(created.id);
    expect(await store.job(created.id)).toMatchObject({
      state: 'delivered',
      prUrl: 'https://github.com/example/project/pull/2',
      inputTokens: 150,
      outputTokens: 50,
      estimatedCostUsd: 0.050014,
    });
  });

  it('lets only one worker claim a paid job without refunding the winner', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'concurrent-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');

    let releaseHead!: () => void;
    let signalStarted!: () => void;
    const headGate = new Promise<void>((resolve) => {
      releaseHead = resolve;
    });
    const started = new Promise<void>((resolve) => {
      signalStarted = resolve;
    });
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-concurrent' });
      if (url.endsWith('/v1/runs/run-concurrent')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return Response.json({
          choices: [{ message: { content: JSON.stringify({ approved: true, reason: 'scoped' }) } }],
          usage: { prompt_tokens: 50, completion_tokens: 10 },
        });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    let headCalls = 0;
    const github = {
      currentHead: async () => {
        headCalls += 1;
        if (headCalls === 1) {
          signalStarted();
          await headGate;
        }
        return jobQuote.baseSha;
      },
      publish: async () => 'https://github.com/example/project/pull/3',
    };
    const config = loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' });
    const processor = new JobProcessor(config, store, github, request as typeof fetch);
    const first = processor.process(created.id);
    await started;
    const second = processor.process(created.id);
    releaseHead();
    await Promise.all([first, second]);

    const completed = await store.job(created.id);
    expect(completed).toMatchObject({ state: 'delivered' });
    expect(completed).not.toHaveProperty('refundTransaction');
    expect((await store.activity()).filter((event) => event.kind === 'job.delivered')).toHaveLength(
      1,
    );
  });

  it('fully refunds a failed paid run in mock mode', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'refund-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-2' });
      if (url.endsWith('/v1/runs/run-2'))
        return Response.json({ status: 'failed', error: 'route failed', costUsd: 0.12 });
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };
    const config = loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' });
    await new JobProcessor(config, store, github, request as typeof fetch).process(created.id);
    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      error: 'route failed',
      refundTransaction: `mock-refund-${created.id}`,
      estimatedCostUsd: 0.12,
    });
  });

  it('retains completed gateway cost when its artifact receipt is unavailable', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'artifact-failure-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-artifact-failure' });
      if (url.endsWith('/v1/runs/run-artifact-failure')) {
        return Response.json({ status: 'completed', costUsd: 0.31 });
      }
      if (url.endsWith('/artifacts')) return Response.json({}, { status: 503 });
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };
    const config = loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' });

    await new JobProcessor(config, store, github, request as typeof fetch).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      error: 'coding gateway artifacts unavailable',
      estimatedCostUsd: 0.31,
    });
  });
});

function gatewayReadiness(
  overrides: Partial<Record<string, unknown>> = {},
): Record<string, unknown> {
  const checkedAt = '2026-08-22T12:00:00.000Z';
  return {
    ready: true,
    checkedAt,
    ageMs: 0,
    lastSuccessfulAt: checkedAt,
    lastSuccessfulAgeMs: 0,
    dependencies: {
      model: { ok: true, checkedAt, latencyMs: 12 },
      sandbox: { ok: true, checkedAt, latencyMs: 24 },
    },
    failed: [],
    backend: 'usepod',
    provider: 'e2b',
    persistentRuns: true,
    ...overrides,
  };
}
