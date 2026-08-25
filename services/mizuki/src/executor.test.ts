import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { enforcePolicy, JobProcessor, phaseBudgetPlan, phaseBudgetUsd } from './executor.js';
import { deliveryDiffHash } from './github.js';
import { MemoryStore } from './store.js';
import { treasurySnapshot } from './treasury.js';
import type { FinancialPolicy, RefundLiability } from './policy-client.js';
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
    const invalidTariff = new JobProcessor(config, new MemoryStore(), github, async () =>
      Response.json(
        gatewayReadiness({
          ready: false,
          dependencies: {
            model: { ok: true, checkedAt: '2026-08-22T12:00:00.000Z', latencyMs: 12 },
            balance: { ok: true, checkedAt: '2026-08-22T12:00:00.000Z', latencyMs: 8 },
            sandbox: { ok: true, checkedAt: '2026-08-22T12:00:00.000Z', latencyMs: 24 },
            tariff: { ok: false, checkedAt: '2026-08-22T12:00:00.000Z', latencyMs: 18 },
          },
          failed: ['tariff'],
        }),
      ),
    );

    await expect(malformed.readiness()).rejects.toThrow();
    await expect(stale.readiness()).rejects.toThrow('readiness evidence is invalid');
    await expect(invalidTariff.readiness()).rejects.toThrow('readiness evidence is invalid');
  });

  it('delivers an independently approved gateway change', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'delivery-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request, init?: RequestInit) => {
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
        expect(JSON.parse(String(init?.body))).toMatchObject({ max_tokens: 512 });
        return reviewResponse(
          { approved: true, reason: 'scoped' },
          'deepseek/deepseek-v4-flash-0731',
        );
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
      estimatedCostUsd: 0.0505,
      reviewReceipt: {
        provider: {
          model: 'deepseek-v4-flash',
          route: 'marketplace',
          providerId: 'provider-1',
          costMicrounits: '500',
        },
      },
    });
  });

  it('refunds when the reviewer returns a nearby canonical model identity', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'review-model-mismatch')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-model-mismatch' });
      if (url.endsWith('/v1/runs/run-model-mismatch')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return reviewResponse(
          { approved: true, reason: 'scoped' },
          'deepseek/deepseek-v4-flash-0730',
        );
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      error: 'UsePod reviewer returned a different model',
      reviewAttempts: [
        {
          phase: 'implementation',
          status: 'failed',
          provider: { model: 'deepseek-v4-flash', route: 'marketplace' },
          error: 'UsePod reviewer returned a different model',
        },
      ],
    });
  });

  it('records the signer delivery binding before publication in live mode', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'live-delivery-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid', {
      refundLiabilityId: '22222222-2222-4222-8222-222222222222',
    });
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-live-delivery' });
      if (url.endsWith('/v1/runs/run-live-delivery')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return reviewResponse({ approved: true, reason: 'scoped' });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const events: string[] = [];
    const diffHash = deliveryDiffHash(artifacts.patch);
    const headSha = 'b'.repeat(40);
    const bindRefundLiabilityDelivery = vi.fn(async (_id, input) => {
      events.push('binding');
      return {
        id: '22222222-2222-4222-8222-222222222222',
        jobId: created.id,
        settlementSignature: payment.transaction,
        reviewedHeadSha: input.reviewedHeadSha,
        reviewedBaseSha: input.reviewedBaseSha,
        reviewedBaseRef: input.reviewedBaseRef,
        reviewedDiffHash: input.reviewedDiffHash,
        deliveryBoundAt: '2026-08-23T12:00:00.000Z',
        deliveryBindingHash: 'c'.repeat(64),
      } as RefundLiability;
    });
    const policy = { bindRefundLiabilityDelivery } as unknown as FinancialPolicy;
    const github = {
      currentHead: async () => jobQuote.baseSha,
      publish: async (
        _job: Job,
        _artifacts: RunArtifacts,
        checkpoint: (sha: string) => Promise<void>,
        evidenceCheckpoint: (evidence: NonNullable<Job['deliveryEvidence']>) => Promise<void>,
      ) => {
        await checkpoint(headSha);
        events.push('published');
        await evidenceCheckpoint({
          pullRequestNumber: 2,
          headSha,
          baseSha: jobQuote.baseSha,
          baseRef: jobQuote.defaultBranch,
          diffHash,
          observedAt: '2026-08-23T12:00:01.000Z',
        });
        return 'https://github.com/example/project/pull/2';
      },
    };
    const config = loadConfig({
      MIZUKI_PAYMENT_MODE: 'live',
      USEPOD_API_KEY: 'test',
      USEPOD_MODEL: 'implementation-model',
      USEPOD_REVIEW_MODEL: 'deepseek-v4-flash',
    });

    await new JobProcessor(
      config,
      store,
      github,
      request as typeof fetch,
      undefined,
      policy,
    ).process(created.id);

    const completed = await store.job(created.id);
    expect(completed).toMatchObject({ state: 'delivered' });
    expect(events).toEqual(['binding', 'published']);
    expect(bindRefundLiabilityDelivery).toHaveBeenCalledWith(
      '22222222-2222-4222-8222-222222222222',
      {
        jobId: created.id,
        settlementSignature: payment.transaction,
        reviewedHeadSha: headSha,
        reviewedBaseSha: jobQuote.baseSha,
        reviewedBaseRef: jobQuote.defaultBranch,
        reviewedDiffHash: diffHash,
      },
    );
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
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return reviewResponse({ approved: true, reason: 'scoped' });
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
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.31,
        });
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

  it('retries a lost creation response with the same durable implementation key', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'lost-response-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const sessions: string[] = [];
    let submissions = 0;
    const request = async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) {
        submissions += 1;
        sessions.push((JSON.parse(String(init?.body)) as { session_id: string }).session_id);
        if (submissions === 1) throw new TypeError('response connection lost');
        return Response.json({ run_id: 'run-replayed' });
      }
      if (url.endsWith('/v1/runs/run-replayed')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return reviewResponse({ approved: true, reason: 'scoped' });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = {
      currentHead: async () => jobQuote.baseSha,
      publish: async () => 'https://github.com/example/project/pull/4',
    };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'delivered',
      runId: 'run-replayed',
    });
    expect(submissions).toBe(2);
    expect(sessions).toEqual([`${created.id}:implementation`, `${created.id}:implementation`]);
  });

  it('charges the implementation phase ceiling when a completed run omits cost', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'missing-cost-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-missing-cost' });
      if (url.endsWith('/v1/runs/run-missing-cost')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return reviewResponse({ approved: true, reason: 'scoped' });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: 0.44,
    });
  });

  it('never allocates more than the quote and refuses a paid follow-up with no remaining cap', async () => {
    const plan = phaseBudgetPlan(quote.maxCostUsd);
    expect(Object.values(plan).reduce((sum, value) => sum + value, 0)).toBeLessThanOrEqual(
      quote.maxCostUsd,
    );
    expect(() =>
      phaseBudgetUsd(quote.maxCostUsd, quote.maxCostUsd, 'implementation-review'),
    ).toThrow(/cap exhausted/);

    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'overrun-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    let reviewCalls = 0;
    let requestedCap = 0;
    const request = async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) {
        requestedCap = (JSON.parse(String(init?.body)) as { max_cost_usd: number }).max_cost_usd;
        return Response.json({ run_id: 'run-overrun' });
      }
      if (url.endsWith('/v1/runs/run-overrun')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: quote.maxCostUsd,
        });
      }
      if (url.endsWith('/chat/completions')) {
        reviewCalls += 1;
        return reviewResponse({ approved: true, reason: 'scoped' });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(requestedCap).toBe(plan.implementation);
    expect(reviewCalls).toBe(0);
    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: quote.maxCostUsd,
    });
  });

  it('persists billed rejected reviewer attempts through repair and refund', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'rejected-review-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    let review = 0;
    const request = async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) {
        const body = JSON.parse(String(init?.body)) as { session_id: string };
        return Response.json({
          run_id: body.session_id.endsWith(':repair') ? 'run-repair' : 'run-implementation',
        });
      }
      if (url.endsWith('/run-implementation') || url.endsWith('/run-repair')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        review += 1;
        return reviewResponse({ approved: false, reason: `rejected-${review}` });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: 0.101,
      reviewAttempts: [
        {
          phase: 'implementation',
          approved: false,
          reason: 'rejected-1',
          costUsd: 0.0005,
          provider: { route: 'marketplace' },
        },
        {
          phase: 'repair',
          approved: false,
          reason: 'rejected-2',
          costUsd: 0.0005,
          provider: { route: 'marketplace' },
        },
      ],
    });
  });

  it('retains a billed reviewer receipt when the response usage is invalid', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'invalid-review-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-invalid-review' });
      if (url.endsWith('/v1/runs/run-invalid-review')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        const response = reviewResponse({ approved: true, reason: 'scoped' });
        const body = (await response.json()) as Record<string, unknown>;
        body.usage = { prompt_tokens: -1, completion_tokens: 10 };
        return Response.json(body, { headers: response.headers });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: 0.0505,
      reviewAttempts: [
        {
          phase: 'implementation',
          costUsd: 0.0005,
          provider: { route: 'marketplace', costMicrounits: '500' },
          error: expect.stringMatching(/invalid token usage/),
        },
      ],
    });
  });

  it('books the full review allocations when provider cost reports are missing', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'missing-review-cost-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    let submissions = 0;
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) {
        submissions += 1;
        return Response.json({ run_id: 'run-missing-review-cost' });
      }
      if (url.endsWith('/v1/runs/run-missing-review-cost')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        return Response.json(
          {
            model: 'deepseek-v4-flash',
            choices: [
              { message: { content: JSON.stringify({ approved: false, reason: 'repair' }) } },
            ],
            usage: { prompt_tokens: 50, completion_tokens: 10 },
          },
          {
            headers: {
              'x-pod-route': 'marketplace',
              'x-balance-remaining': '9000000',
            },
          },
        );
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };

    await new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    ).process(created.id);

    expect(submissions).toBe(2);
    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: 0.3,
      reviewAttempts: [
        {
          phase: 'implementation',
          costUsd: 0.12,
          status: 'completed',
          provider: { route: 'marketplace' },
        },
        {
          phase: 'repair',
          costUsd: 0.08,
          status: 'completed',
          provider: { route: 'marketplace' },
        },
      ],
    });
    expect(
      (await store.job(created.id))?.reviewAttempts?.every(
        (attempt) => attempt.provider?.costMicrounits === undefined,
      ),
    ).toBe(true);
  });

  it('retains a pending review reservation through restart reconciliation and refund', async () => {
    const store = new MemoryStore();
    const jobQuote = { ...quote, validationCommands: [] };
    const created = (await store.createJob(jobQuote, payment, 'review-crash-key')).job;
    await store.transitionJob(created.id, 'settlement_pending', 'paid');
    let rejectReview!: (cause: Error) => void;
    let reviewStarted!: () => void;
    const started = new Promise<void>((resolve) => {
      reviewStarted = resolve;
    });
    const request = async (input: string | URL | Request) => {
      const url = String(input);
      if (url.endsWith('/v1/runs')) return Response.json({ run_id: 'run-review-crash' });
      if (url.endsWith('/v1/runs/run-review-crash')) {
        return Response.json({
          status: 'completed',
          usage: { inputTokens: 100, outputTokens: 40 },
          costUsd: 0.05,
        });
      }
      if (url.endsWith('/artifacts')) return Response.json({ ...artifacts, validations: [] });
      if (url.endsWith('/chat/completions')) {
        reviewStarted();
        return new Promise<Response>((_resolve, reject) => {
          rejectReview = reject;
        });
      }
      throw new Error(`unexpected request: ${url}`);
    };
    const github = { currentHead: async () => jobQuote.baseSha, publish: async () => '' };
    const processor = new JobProcessor(
      loadConfig({ MIZUKI_PAYMENT_MODE: 'mock', USEPOD_API_KEY: 'test' }),
      store,
      github,
      request as typeof fetch,
    );

    const processing = processor.process(created.id);
    await started;
    expect(await store.job(created.id)).toMatchObject({
      state: 'validating',
      estimatedCostUsd: 0.17,
      reviewAttempts: [{ status: 'pending', costUsd: 0.12 }],
    });
    expect((await store.job(created.id))?.reviewAttempts?.[0]?.provider).toBeUndefined();

    await processor.reconcileInFlight(-1);
    rejectReview(new Error('connection lost after provider accepted request'));
    await processing;

    expect(await store.job(created.id)).toMatchObject({
      state: 'refunded',
      estimatedCostUsd: 0.17,
      reviewAttempts: [{ status: 'failed', costUsd: 0.12 }],
    });
    expect((await store.job(created.id))?.reviewAttempts?.[0]?.provider).toBeUndefined();
    const routeCost = (await store.ledgerEntries()).find((entry) => entry.kind === 'route_cost');
    expect(routeCost?.amountUsd).toBe(0.17);
    expect((await treasurySnapshot(store)).trailingVariableAndOperatingEstimateUsd).toBe(0.17);
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
      balance: { ok: true, checkedAt, latencyMs: 8 },
      sandbox: { ok: true, checkedAt, latencyMs: 24 },
      tariff: { ok: true, checkedAt, latencyMs: 18 },
    },
    failed: [],
    model: 'deepseek-v3.2',
    backend: 'usepod',
    provider: 'e2b',
    persistentRuns: true,
    storage: { ledger: true, runStore: true },
    ...overrides,
  };
}

function reviewResponse(
  decision: { approved: boolean; reason: string },
  model = 'deepseek-v4-flash',
): Response {
  return Response.json(
    {
      model,
      choices: [{ message: { content: JSON.stringify(decision) } }],
      usage: { prompt_tokens: 50, completion_tokens: 10 },
    },
    {
      headers: {
        'x-pod-route': 'marketplace',
        'x-balance-remaining': '9000000',
        'x-pod-provider-id': 'provider-1',
        'x-balance-cost-microunits': '500',
      },
    },
  );
}
