import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ComputeCleanupReport, ComputeConfig, ComputeSessionOptions } from '../src/compute.js';
import type { RunOptions } from '../src/types.js';

const nativeFetch = globalThis.fetch;
const token = 'gpu-test-auth-'.repeat(4);
const policy = { max_usdc_micros: 200000, duration_secs: 300, max_launches: 1 } as const;
const directories: string[] = [];
const servers: Server[] = [];
const readers: ReadableStreamDefaultReader<Uint8Array>[] = [];

const pending = (runKey: string): ComputeCleanupReport => ({
  runKey,
  complete: false,
  settled: [],
  pendingJobIds: ['smoke-job'],
  pendingLaunchKeys: [],
});
const settled = (runKey: string): ComputeCleanupReport => ({
  runKey,
  complete: true,
  pendingJobIds: [],
  pendingLaunchKeys: [],
  settled: [
    {
      job_id: 'smoke-job',
      status: 'cancelled',
      maximum_usdc_micros: 100000,
      receipt: {
        job_id: 'smoke-job',
        runtime_secs: 30,
        charged_usdc_micros: 10000,
        refunded_usdc_micros: 90000,
      },
    },
  ],
});

afterEach(async () => {
  for (const reader of readers.splice(0)) await reader.cancel().catch(() => {});
  for (const server of servers.splice(0)) {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
  for (const path of [
    '../src/compute.js',
    '../src/readiness.js',
    '../src/backends/index.js',
    '../src/sandbox/e2b.js',
    '../src/run-store.js',
    '../src/workspace-exec.js',
  ])
    vi.doUnmock(path);
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.resetModules();
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

async function boot(
  options: {
    enabled?: boolean;
    auth?: string;
    isolated?: boolean;
    failTerminalPersist?: boolean;
    recovered?: boolean;
    stored?: unknown[];
    allowedRunKey?: string;
    directory?: string;
    computeFetch?: (url: string, init?: RequestInit) => unknown | Promise<unknown>;
    backendFailure?: boolean;
    backendOutput?: string;
    /** Verified sandbox list price per second; unset leaves the tariff unverified. */
    tariffUsdPerSec?: number;
    /** Runs inside the mocked backend after launch, with the real GPU runner. */
    gpuCommand?: (opts: RunOptions) => Promise<unknown>;
    /** What the GPU workspace returns for a command; no Jupyter is contacted. */
    workspaceOutcome?: import('../src/workspace-exec.js').WorkspaceJobOutcome;
    workspaceError?: Error;
  } = {},
) {
  vi.resetModules();
  const directory = options.directory ?? mkdtempSync(join(tmpdir(), 'gpu-server-'));
  if (!options.directory) directories.push(directory);
  const runPath = join(directory, 'runs.json');
  for (const [name, value] of Object.entries({
    NODE_ENV: 'test',
    CODER_AUTH_TOKEN: options.auth ?? token,
    CODER_ALLOWED_RUN_KEY: options.allowedRunKey,
    CODER_BACKEND: 'anthropic',
    CODER_MODEL: 'claude-sonnet-4-6',
    ANTHROPIC_API_KEY: 'unused-provider-test-key',
    E2B_API_KEY: options.isolated === false ? '' : 'unused-e2b-test-key',
    COMPUTE_API_TOKEN: 'unused-compute-test-key',
    COMPUTE_API_URL: 'https://compute.invalid',
    COMPUTE_GPU_ENABLED: options.enabled === false ? '0' : '1',
    COMPUTE_DURABLE_STORAGE: '1',
    COMPUTE_JOURNAL_PATH: join(directory, 'gpu.json'),
    RUN_STORE_PATH: runPath,
    LEDGER_PATH: join(directory, 'ledger.json'),
    COMPUTE_MAX_USDC_MICROS: '200000',
    COMPUTE_MAX_DURATION_SECS: '300',
    COMPUTE_MAX_LAUNCHES: '1',
    CODER_IP_MAX_PER_IP: '0',
    E2B_TEMPLATE_ID: options.tariffUsdPerSec ? 'tpl_lifecycle_test' : '',
    CODER_E2B_TARIFF_REF: options.tariffUsdPerSec
      ? `https://evidence.invalid/e2b.json#sha256=${'a'.repeat(64)}`
      : undefined,
    COMPUTE_ARTIFACT_BASE_URL: 'https://connector.invalid/artifacts',
  }))
    vi.stubEnv(name, value);
  if (options.stored) writeFileSync(runPath, JSON.stringify(options.stored));

  // No model, sandbox, or compute network can escape this test. Only calls to
  // the local HTTP server below use the native fetch implementation.
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      if (options.computeFetch && String(url).startsWith('https://compute.invalid/')) {
        const body = await options.computeFetch(String(url), init);
        return body instanceof Response ? body : new Response(JSON.stringify(body));
      }
      throw new Error('unexpected external request');
    }),
  );
  const reports = new Map<string, ComputeCleanupReport>();
  const constructions: Array<{ cfg: ComputeConfig; options: ComputeSessionOptions }> = [];
  const launch = vi.fn(async (key: string) => {
    reports.set(key, pending(key));
    return { id: 'smoke-job' };
  });
  const reap = vi.fn(async (key: string) => structuredClone(reports.get(key)!));
  if (options.computeFetch) vi.doUnmock('../src/compute.js');
  else
    vi.doMock('../src/compute.js', async (importOriginal) => {
      const actual = await importOriginal<typeof import('../src/compute.js')>();
      return {
        ...actual,
        ComputeSession: class {
          readonly key: string;
          constructor(cfg: ComputeConfig, opts: ComputeSessionOptions) {
            constructions.push({ cfg, options: opts });
            this.key = opts.runKey;
            if (!reports.has(this.key))
              reports.set(this.key, {
                runKey: this.key,
                complete: true,
                settled: [],
                pendingJobIds: [],
                pendingLaunchKeys: [],
              });
          }
          launch() {
            return launch(this.key);
          }
          inspect() {
            return structuredClone(reports.get(this.key)!);
          }
          reap() {
            return reap(this.key);
          }
          async awaitRunning() {
            return {
              accessUrl: `https://127.0.0.1:9/lab?token=${'t'.repeat(32)}`,
              readySinceMs: Date.now(),
              durationSecs: 300,
            };
          }
          static async recover() {
            return { runs: [] };
          }
        },
      };
    });
  let ready = true;
  const check = vi.fn(async () => ({
    ready,
    failed: ready ? [] : ['model'],
    dependencies: {},
  }));
  vi.doMock('../src/readiness.js', () => ({
    GatewayReadiness: class {
      constructor(private readonly opts: { tariff?: { check: () => Promise<unknown> } }) {}
      check = async () => {
        await this.opts.tariff?.check();
        return check();
      };
    },
    verifyE2bTariff: vi.fn(async () => ({
      validUntilMs: Date.now() + 3_600_000,
      listUsdPerSec: options.tariffUsdPerSec,
    })),
  }));
  const runWorkspaceJob = vi.fn(async () => {
    if (options.workspaceError) throw options.workspaceError;
    return options.workspaceOutcome!;
  });
  if (options.workspaceOutcome || options.workspaceError)
    vi.doMock('../src/workspace-exec.js', async (importOriginal) => ({
      ...(await importOriginal<typeof import('../src/workspace-exec.js')>()),
      runWorkspaceJob,
    }));
  const sandbox = {
    exec: vi.fn(async () => ({ stdout: '', stderr: '', exitCode: 0 })),
    readFile: vi.fn(async () => ''),
    writeFile: vi.fn(async () => {}),
    previewUrl: vi.fn(async () => ''),
    destroy: vi.fn(async () => {}),
  };
  const createSandbox = vi.fn(async () => sandbox);
  vi.doMock('../src/sandbox/e2b.js', () => ({
    E2bSandboxProvider: class {
      readonly id = 'e2b';
      create = createSandbox;
      async check() {}
    },
  }));
  const backend = vi.fn(async (opts: RunOptions) => {
    if (opts.computeSession)
      await opts.computeSession.launch().catch((error: unknown) => {
        // The model can handle a failed GPU tool result and finish its E2B work.
        if (!options.computeFetch) throw error;
      });
    if (options.gpuCommand) await options.gpuCommand(opts);
    if (options.backendFailure) {
      await opts.recordProviderRequest?.();
      throw new Error('model reply outcome unknown');
    }
    opts.emit({ type: 'message.delta', text: 'working' });
    // Even a legacy backend's premature terminal event must be suppressed.
    opts.emit({ type: 'run.completed', output: 'premature backend success' });
    return {
      output: options.backendOutput ?? 'task done',
      usage: { inputTokens: 100, outputTokens: 10, cacheReadTokens: 0, cacheCreationTokens: 0 },
    };
  });
  vi.doMock('../src/backends/index.js', () => ({
    selectBackend: () => ({ id: 'anthropic', run: backend }),
  }));
  if (options.failTerminalPersist) {
    let fail = true;
    vi.doMock('../src/run-store.js', async (importOriginal) => {
      const actual = await importOriginal<typeof import('../src/run-store.js')>();
      return {
        ...actual,
        RunStore: class extends actual.RunStore {
          override save(run: import('../src/run-store.js').StoredRun) {
            if (fail && run.status === 'completed') {
              fail = false;
              throw new Error('injected receipt persistence failure');
            }
            return super.save(run);
          }
        },
      };
    });
  }
  const gateway = await import('../src/server.js');
  servers.push(gateway.server);
  await new Promise<void>((resolve) => gateway.server.listen(0, '127.0.0.1', resolve));
  const address = gateway.server.address();
  if (!address || typeof address === 'string') throw new Error('test gateway did not bind');
  const origin = `http://127.0.0.1:${address.port}`;
  const headers = {
    authorization: `Bearer ${options.auth ?? token}`,
    'content-type': 'application/json',
  };
  const get = (path: string) => nativeFetch(`${origin}${path}`, { headers });
  const post = (body: unknown) =>
    nativeFetch(`${origin}/v1/runs`, { headers, method: 'POST', body: JSON.stringify(body) });
  if (options.recovered !== false) await gateway.recoverCompute();
  return {
    ...gateway,
    origin,
    headers,
    get,
    post,
    reports,
    constructions,
    launch,
    reap,
    backend,
    createSandbox,
    sandbox,
    runWorkspaceJob,
    check,
    runPath,
    directory,
    setReady(value: boolean) {
      ready = value;
    },
  };
}

async function untilStatus(gateway: Awaited<ReturnType<typeof boot>>, id: string, status: string) {
  for (let i = 0; i < 100; i++) {
    const state = (await (await gateway.get(`/v1/runs/${id}`)).json()) as Record<string, any>;
    if (state.status === status) return state;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`run did not reach ${status}`);
}

const commandTimings = {
  workspaceReadyWaitMs: 101,
  stagingMs: 203,
  commandBudgetMs: 250_000,
  commandElapsedMs: 41_200,
  collectionMs: 307,
};
const completedCommand = (
  overrides: Partial<import('../src/workspace-exec.js').WorkspaceJobOutcome> = {},
): import('../src/workspace-exec.js').WorkspaceJobOutcome => ({
  exitCode: 0,
  timedOut: false,
  durationMs: 41_200,
  timings: commandTimings,
  stdoutTail: 'command finished',
  stderrTail: '',
  outputs: [{ path: 'report.json', bytes: Buffer.from('{}') }],
  missing: [],
  skipped: [],
  ...overrides,
});

const request = (id: string, gpu = true) => ({
  session_id: id,
  input: 'GPU smoke test',
  max_cost_usd: 0.5,
  ...(gpu ? { gpu: policy } : {}),
});

describe('GPU gateway authorization and lifecycle', () => {
  it('preserves complete receipt and accounting evidence in long output, persisted state, and terminal SSE', async () => {
    const gateway = await boot({ backendOutput: 'Long model explanation. '.repeat(1_000) });
    const submitted = await gateway.post(request('long-output'));
    const { run_id } = (await submitted.json()) as { run_id: string };
    await untilStatus(gateway, run_id, 'stopping');
    gateway.reports.set('long-output', settled('long-output'));
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'completed');
    expect(final.output.length).toBeLessThanOrEqual(8_000);
    expect(final.output).toContain('[model output truncated]');
    expect(final.output).toContain('[gpu_workspace final receipts:');
    const accounting = JSON.parse(final.output.match(/\[run accounting: (.*)\]$/)[1]);
    // Without verified tariff evidence the sandbox line stays at its reservation.
    expect(accounting).toMatchObject({
      version: 2,
      final: false,
      gpu: { usd: 0.01 },
      totalUsd: 0.13045,
      sandbox: { basis: 'maximum_reservation' },
    });
    const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(saved.output).toBe(final.output);
    expect(
      saved.events.find((event: { type: string }) => event.type === 'run.completed').output,
    ).toBe(final.output);
    const sse = await (await gateway.get(`/v1/runs/${run_id}/events`)).text();
    const terminal = sse
      .split('\n')
      .filter((line) => line.startsWith('data: '))
      .map((line) => JSON.parse(line.slice(6)))
      .find((event) => event.event === 'run.completed');
    expect(terminal.output).toBe(final.output);
    expect(gateway.createSandbox).toHaveBeenCalledOnce();
    expect(gateway.launch).toHaveBeenCalledOnce();
  });

  it('automatically settles a lost GPU response with model and E2B accounting after cancellation', async () => {
    let recoverable = false;
    let key = '';
    let posts = 0;
    let job: Record<string, unknown>;
    const gateway = await boot({
      computeFetch: (url, init) => {
        if (url.endsWith('/v1/offers'))
          return [
            {
              id: 'vast:test-offer',
              gpu: { model: 'RTX 4090', vram_mib: 24_576 },
              rate_usdc_micros_per_hour: 1_200_000,
              online: true,
            },
          ];
        if (url.endsWith('/v1/jobs') && init?.method === 'POST') {
          posts++;
          const plan = JSON.parse(String(init.body));
          key = (init.headers as Record<string, string>)['Idempotency-Key'];
          job = {
            id: 'accepted-before-response-loss',
            status: 'stopping',
            offer_id: plan.offer.id,
            maximum_usdc_micros: plan.maximum_usdc_micros,
            access_url: null,
            error: null,
            receipt: null,
          };
          return new Response(JSON.stringify({ error: { code: 'provider_unavailable' } }), {
            status: 503,
          });
        }
        if (url.endsWith(`/v1/jobs/by-key/${key}`) && init?.method === 'DELETE')
          return recoverable
            ? { idempotency_key: key, cancellation_recorded: true, job }
            : new Response('{}', { status: 503 });
        if (url.endsWith('/v1/jobs/accepted-before-response-loss') && init?.method === 'DELETE')
          return {
            ...job,
            status: 'cancelled',
            receipt: {
              job_id: job.id,
              runtime_secs: 30,
              charged_usdc_micros: 10_000,
              refunded_usdc_micros: 90_000,
              provisioning_secs: 20,
              provisioning_usdc_micros: 6_667,
              commitment: 'vast:instance:test:destroyed',
            },
          };
        throw new Error('unexpected compute request');
      },
    });
    const body = request('lost-response-lifecycle');
    const submitted = await gateway.post(body);
    const { run_id } = (await submitted.json()) as { run_id: string };
    const stopping = await untilStatus(gateway, run_id, 'stopping');
    // The first finally/reaper pass also finishes before inspecting the journal.
    await gateway.reconcileCompute();
    expect(stopping).toMatchObject({ compute: { complete: false, pendingLaunchKeys: [key] } });
    expect(stopping.costUsd).toBeUndefined();
    expect(gateway.sandbox.destroy).toHaveBeenCalledOnce();
    expect(
      await (
        await nativeFetch(`${gateway.origin}/v1/runs/${run_id}/stop`, {
          method: 'POST',
          headers: gateway.headers,
        })
      ).json(),
    ).toMatchObject({ status: 'stopping' });
    recoverable = true;
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'cancelled');
    expect(final.compute).toMatchObject({
      complete: true,
      pendingLaunchKeys: [],
      pendingJobIds: [],
      settled: [
        {
          job_id: 'accepted-before-response-loss',
          status: 'cancelled',
          receipt: {
            charged_usdc_micros: 10_000,
            refunded_usdc_micros: 90_000,
            provisioning_usdc_micros: 6_667,
            commitment: 'vast:instance:test:destroyed',
          },
        },
      ],
    });
    expect(final.costUsd).toBeCloseTo(0.13045, 6);
    expect(final.output).toContain('gpu_workspace final receipts');
    const accounting = JSON.parse(final.output.match(/\[run accounting: (.*)\]$/)[1]);
    expect(accounting).toMatchObject({
      version: 2,
      final: false,
      model: {
        usd: 0.00045,
        basis: 'usage_at_list_price',
        name: 'claude-sonnet-4-6',
        usage: { inputTokens: 100, outputTokens: 10 },
      },
      sandbox: { usd: 0.12, basis: 'maximum_reservation', seconds: 600 },
      gpu: { usd: 0.01, runtimeSecs: 30, jobs: ['accepted-before-response-loss'] },
      coveredByCovenant: { gpuProvisioningUsd: 0.006667, gpuProvisioningSecs: 20 },
      totalUsd: 0.13045,
    });
    const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(saved.nonComputeUsd).toBeCloseTo(0.12045, 6);
    expect(saved.usage).toMatchObject({ inputTokens: 100, outputTokens: 10 });
    expect(
      saved.events.filter((event: { type: string }) => event.type === 'run.failed'),
    ).toHaveLength(1);
    expect(await (await gateway.get('/v1/budget')).json()).toMatchObject({
      reserved: 0,
      active: 0,
      dailyUsd: 0.13045,
    });
    const journal = JSON.parse(readFileSync(join(gateway.directory, 'gpu.json'), 'utf8'));
    const reservation = Object.values(journal.runs)[0] as Record<string, any>;
    expect(reservation).toMatchObject({ closed: true, maxLaunches: 1 });
    expect(reservation.reservations).toHaveLength(1);
    expect(posts).toBe(1);
    expect(gateway.backend).toHaveBeenCalledOnce();
    expect(gateway.createSandbox).toHaveBeenCalledOnce();
    expect(await (await gateway.post(body)).json()).toEqual({ run_id });
    await gateway.reconcileCompute();
    expect(posts).toBe(1);
  });

  it('reports a conservative model hold and unknown usage after a lost model reply', async () => {
    const gateway = await boot({ backendFailure: true });
    const response = await gateway.post(request('unknown-model-usage'));
    const { run_id } = (await response.json()) as { run_id: string };
    await untilStatus(gateway, run_id, 'stopping');
    gateway.reports.set('unknown-model-usage', settled('unknown-model-usage'));
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'failed');
    const accounting = JSON.parse(final.output.match(/\[run accounting: (.*)\]$/)[1]);
    expect(accounting.model).toEqual({ usd: 0.18, basis: 'conservative_hold', usage: null });
    expect(accounting.sandbox).toMatchObject({ usd: 0.12, basis: 'maximum_reservation' });
    expect(accounting.final).toBe(false);
    expect(accounting.totalUsd).toBe(0.31);
    expect(final.costUsd).toBe(0.31);
    expect(gateway.sandbox.destroy).toHaveBeenCalledOnce();
  });

  it.each([
    ['timeout', { exitCode: null, timedOut: true, outputs: [], missing: ['report.json'] }],
    ['unknown_exit', { exitCode: null }],
    ['nonzero_exit', { exitCode: 2 }],
    ['missing_outputs', { outputs: [], missing: ['report.json'] }],
    ['oversized_outputs', { outputs: [], skipped: ['report.json'] }],
    ['execution_error', null],
  ] as const)(
    'fails %s despite normal model completion, after cleanup, with durable outcome and full receipt',
    async (failure, result) => {
      let toolError: unknown;
      const gateway = await boot({
        workspaceOutcome: result ? completedCommand(JSON.parse(JSON.stringify(result))) : undefined,
        workspaceError: result ? undefined : new Error('workspace network failed'),
        backendOutput: 'All files were produced successfully. '.repeat(1_000),
        gpuCommand: async (opts) => {
          // The real Anthropic loop reports a failed tool and may still return
          // normal model prose. That prose cannot override the durable command.
          try {
            await opts.gpuRunner!.run(
              { job_id: 'smoke-job', command: 'python embed.py', outputs: ['report.json'] },
              opts.sandbox,
            );
          } catch (error) {
            toolError = error;
          }
        },
      });
      const key = `failure-${failure}`;
      const { run_id } = (await (await gateway.post(request(key))).json()) as { run_id: string };
      const stopping = await untilStatus(gateway, run_id, 'stopping');
      expect(toolError).toBeDefined();
      expect(stopping.gpuCommands).toHaveLength(1);
      expect(stopping.gpuCommands[0]).toMatchObject({ status: 'failed', failure });
      expect(stopping.compute.complete).toBe(false);
      expect(stopping.usage).toMatchObject({ inputTokens: 100, outputTokens: 10 });
      gateway.reports.set(key, settled(key));
      await gateway.reconcileCompute();
      const final = await untilStatus(gateway, run_id, 'failed');
      expect(final.error).toContain('GPU workload did not succeed');
      expect(final.output.length).toBeLessThanOrEqual(8_000);
      expect(final.output).toContain('[model output truncated]');
      const workload = JSON.parse(final.output.match(/\[gpu_workspace workload: (.*)\]\n/)[1]);
      expect(workload).toMatchObject({
        status: 'failed',
        failures: [failure],
        outputs_checked: 'command_declared_only',
      });
      if (result) expect(workload.outcome.timings).toMatchObject(commandTimings);
      expect(final.output).toContain('[gpu_workspace final receipts:');
      const accounting = JSON.parse(final.output.match(/\[run accounting: (.*)\]$/)[1]);
      expect(accounting).toMatchObject({
        model: { usd: 0.00045, basis: 'usage_at_list_price' },
        sandbox: { usd: 0.12, basis: 'maximum_reservation' },
        gpu: { usd: 0.01 },
        totalUsd: 0.13045,
      });
      expect(final.costUsd).toBe(0.13045);
      expect(gateway.launch).toHaveBeenCalledOnce();
      expect(gateway.sandbox.destroy).toHaveBeenCalledOnce();
      const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
      expect(saved.gpuCommands).toEqual(final.gpuCommands);
      expect(saved.events.at(-1)).toMatchObject({ type: 'run.failed', output: final.output });
      const stream = await (await gateway.get(`/v1/runs/${run_id}/events`)).text();
      const event = stream
        .split('\n')
        .filter((line) => line.startsWith('data: '))
        .map((line) => JSON.parse(line.slice(6)))
        .find((event) => event.event === 'run.failed');
      expect(event.output).toBe(final.output);
    },
  );

  it('keeps the complete receipt and all output links when diagnostics need extensive JSON escaping', async () => {
    const paths = Array.from({ length: 8 }, (_, index) => `${index}-${'x'.repeat(121)}.json`);
    const gateway = await boot({
      backendOutput: 'long explanation '.repeat(1_000),
      workspaceOutcome: completedCommand({
        outputs: paths.map((path) => ({ path, bytes: Buffer.from('{}') })),
        stdoutTail: '\u0000'.repeat(2_000),
        stderrTail: '\u0000'.repeat(2_000),
      }),
      gpuCommand: (opts) =>
        opts.gpuRunner!.run(
          { job_id: 'smoke-job', command: 'python embed.py', outputs: paths },
          opts.sandbox,
        ),
    });
    const { run_id } = (await (await gateway.post(request('bounded-diagnostics'))).json()) as {
      run_id: string;
    };
    await untilStatus(gateway, run_id, 'stopping');
    gateway.reports.set('bounded-diagnostics', settled('bounded-diagnostics'));
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'completed');
    expect(final.output.length).toBeLessThanOrEqual(8_000);
    expect(final.output).toContain('[gpu_workspace workload:');
    expect(final.output).toContain('[gpu_workspace final receipts:');
    expect(final.output).toContain('[run accounting:');
    expect(JSON.parse(final.output.match(/\[gpu_workspace outputs: (.*)\]\n/)[1])).toHaveLength(8);
    expect(final.gpuCommands[0].stdout_tail).toHaveLength(2_000);
    expect(final.gpuCommands[0].stderr_tail).toHaveLength(2_000);
  });

  it('preserves explicit cancellation while retaining the failed workload evidence', async () => {
    const gateway = await boot({
      workspaceOutcome: completedCommand({
        exitCode: null,
        timedOut: true,
        outputs: [],
        missing: ['report.json'],
      }),
      gpuCommand: async (opts) => {
        await opts
          .gpuRunner!.run(
            { job_id: 'smoke-job', command: 'python embed.py', outputs: ['report.json'] },
            opts.sandbox,
          )
          .catch(() => {});
      },
    });
    const { run_id } = (await (await gateway.post(request('cancel-failed-command'))).json()) as {
      run_id: string;
    };
    await untilStatus(gateway, run_id, 'stopping');
    const stopped = await nativeFetch(`${gateway.origin}/v1/runs/${run_id}/stop`, {
      method: 'POST',
      headers: gateway.headers,
    });
    expect(stopped.status).toBe(200);
    gateway.reports.set('cancel-failed-command', settled('cancel-failed-command'));
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'cancelled');
    expect(final.gpuCommands[0]).toMatchObject({ status: 'failed', failure: 'timeout' });
    expect(final.output).toContain('[gpu_workspace final receipts:');
    expect(final.costUsd).toBe(0.13045);
  });

  it('publishes GPU outputs as download links and reports a final metered receipt', async () => {
    const embeddings = Buffer.from(Array.from({ length: 2_048 }, (_, i) => i % 251));
    const report = Buffer.from('{"rows":1000,"gpu":"NVIDIA L40S"}');
    let toolResult: Record<string, any> = {};
    const gateway = await boot({
      tariffUsdPerSec: 0.00003025,
      workspaceOutcome: {
        exitCode: 0,
        timedOut: false,
        durationMs: 41_200,
        timings: commandTimings,
        stdoutTail: 'embedded 1000 lines',
        stderrTail: '',
        outputs: [
          { path: 'out/embeddings.npy', bytes: embeddings },
          { path: 'report.json', bytes: report },
        ],
        missing: [],
        skipped: [],
      },
      gpuCommand: async (opts) => {
        toolResult = JSON.parse(
          await opts.gpuRunner!.run(
            {
              job_id: 'smoke-job',
              command: 'python3 embed.py',
              outputs: ['out/embeddings.npy', 'report.json'],
            },
            opts.sandbox,
          ),
        );
      },
    });
    const { run_id } = (await (await gateway.post(request('gpu-outputs'))).json()) as {
      run_id: string;
    };
    await untilStatus(gateway, run_id, 'stopping');
    gateway.reports.set('gpu-outputs', settled('gpu-outputs'));
    await gateway.reconcileCompute();
    const final = await untilStatus(gateway, run_id, 'completed');

    expect(toolResult).toMatchObject({ exit_code: 0, timed_out: false, duration_secs: 41.2 });
    // Small text outputs come back to the model in full; binaries only by hash.
    expect(toolResult.outputs[1]).toMatchObject({ path: 'report.json', text: report.toString() });
    expect(toolResult.outputs[0].text).toBeUndefined();

    const outputs = JSON.parse(final.output.match(/\[gpu_workspace outputs: (.*)\]\n/)[1]);
    expect(outputs).toHaveLength(2);
    expect(outputs[0]).toMatchObject({
      job_id: 'smoke-job',
      path: 'out/embeddings.npy',
      bytes: embeddings.length,
      sha256: createHash('sha256').update(embeddings).digest('hex'),
    });
    const link = new URL(outputs[0].url);
    expect(link.origin).toBe('https://connector.invalid');
    const download = await nativeFetch(`${gateway.origin}${link.pathname}`);
    expect(download.status).toBe(200);
    expect(download.headers.get('x-content-sha256')).toBe(outputs[0].sha256);
    expect(Buffer.from(await download.arrayBuffer()).equals(embeddings)).toBe(true);
    const guessed = link.pathname.replace(/\/artifacts\/[^/]+\//, `/artifacts/${'A'.repeat(32)}/`);
    expect((await nativeFetch(`${gateway.origin}${guessed}`)).status).toBe(404);

    const accounting = JSON.parse(final.output.match(/\[run accounting: (.*)\]$/)[1]);
    expect(accounting).toMatchObject({
      final: true,
      gpu: { usd: 0.01 },
      model: { usd: 0.00045, basis: 'usage_at_list_price' },
      sandbox: { basis: 'metered_at_list_price', usdPerSec: 0.00003025 },
    });
    expect(accounting.sandbox.seconds).toBeGreaterThanOrEqual(1);
    expect(accounting.totalUsd).toBe(Number((0.01 + 0.00045 + accounting.sandbox.usd).toFixed(9)));
    const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(saved.gpuOutputs).toEqual(outputs);
    expect(gateway.runWorkspaceJob).toHaveBeenCalledOnce();
  });

  it('rejects every different run key before readiness, reservation, or provider work', async () => {
    const gateway = await boot({ allowedRunKey: 'a'.repeat(64) });
    gateway.setReady(false);
    for (const gpu of [true, false]) {
      const response = await gateway.post(request('b'.repeat(64), gpu));
      expect(response.status).toBe(403);
      expect(await response.json()).toEqual({
        error: 'run key is not authorized for this gateway',
      });
    }
    expect(gateway.check).not.toHaveBeenCalled();
    expect(gateway.createSandbox).not.toHaveBeenCalled();
    expect(gateway.backend).not.toHaveBeenCalled();
    expect(gateway.launch).not.toHaveBeenCalled();
    expect(JSON.parse(readFileSync(gateway.runPath, 'utf8'))).toEqual([]);
    expect(await (await gateway.get('/v1/budget')).json()).toMatchObject({
      reserved: 0,
      active: 0,
      dailyUsd: 0,
    });
  });

  it('replays the pinned run across restart without a new sandbox, model turn, or GPU booking', async () => {
    const key = 'a'.repeat(64);
    const body = request(key);
    const gateway = await boot({ allowedRunKey: key });
    const response = await gateway.post(body);
    expect(response.status).toBe(200);
    const { run_id } = (await response.json()) as { run_id: string };
    await untilStatus(gateway, run_id, 'stopping');
    const replay = await gateway.post(body);
    expect(replay.status).toBe(200);
    expect(await replay.json()).toEqual({ run_id });
    expect((await gateway.post({ ...body, input: 'different work' })).status).toBe(409);
    gateway.reports.set(key, settled(key));
    await gateway.reconcileCompute();
    await untilStatus(gateway, run_id, 'completed');
    expect(gateway.createSandbox).toHaveBeenCalledOnce();
    expect(gateway.backend).toHaveBeenCalledOnce();
    expect(gateway.launch).toHaveBeenCalledOnce();
    gateway.server.closeAllConnections();
    await new Promise<void>((resolve) => gateway.server.close(() => resolve()));

    const restarted = await boot({
      allowedRunKey: key,
      directory: gateway.directory,
      enabled: false,
    });
    restarted.setReady(false);
    const restored = await restarted.post(body);
    expect(restored.status).toBe(200);
    expect(await restored.json()).toEqual({ run_id });
    expect((await restarted.post(request('b'.repeat(64)))).status).toBe(403);
    expect((await restarted.post({ ...body, max_cost_usd: 0.6 })).status).toBe(409);
    expect(restarted.check).not.toHaveBeenCalled();
    expect(restarted.createSandbox).not.toHaveBeenCalled();
    expect(restarted.backend).not.toHaveBeenCalled();
    expect(restarted.launch).not.toHaveBeenCalled();
    expect(await (await restarted.get('/v1/budget')).json()).toMatchObject({
      reserved: 0,
      active: 0,
      dailyUsd: 0.13045,
    });
  });

  it('advertises GPU only after recovery, opt-in, and authentication configuration', async () => {
    const gateway = await boot({ recovered: false });
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: false },
    });
    await gateway.recoverCompute();
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: true },
      compute: { budgetUsd: 0.2, maxLaunches: 1, maxDurationSecs: 300 },
    });
    const unauthenticated = await nativeFetch(`${gateway.origin}/v1/runs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(request('unauthorized')),
    });
    expect(unauthenticated.status).toBe(401);
    vi.stubEnv('COMPUTE_GPU_ENABLED', '0');
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: false },
    });
    expect((await gateway.post(request('disabled'))).status).toBe(503);
    expect(gateway.backend).not.toHaveBeenCalled();
    expect(gateway.launch).not.toHaveBeenCalled();
  });

  it('keeps GPU unavailable with an undersized configured authentication token', async () => {
    const gateway = await boot({ auth: 'short-token' });
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: false },
    });
    expect((await gateway.post(request('weak-auth'))).status).toBe(503);
    expect(gateway.backend).not.toHaveBeenCalled();
  });

  it('blocks even ordinary host-shell runs while compute credentials are present', async () => {
    const gateway = await boot({ isolated: false });
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: false },
      sandbox: { provider: 'local' },
    });
    const response = await gateway.post(request('host-shell', false));
    expect(response.status).toBe(503);
    expect(await response.json()).toMatchObject({
      error: expect.stringContaining('isolated sandbox'),
    });
    expect(gateway.backend).not.toHaveBeenCalled();
    expect(gateway.launch).not.toHaveBeenCalled();
  });

  it('does not activate a GPU session without an explicit exact dispatch policy', async () => {
    const gateway = await boot();
    const ordinary = await gateway.post(request('ordinary', false));
    expect(ordinary.status).toBe(200);
    const { run_id } = (await ordinary.json()) as { run_id: string };
    await untilStatus(gateway, run_id, 'completed');
    expect(gateway.backend).toHaveBeenCalledTimes(1);
    expect(gateway.backend.mock.calls[0][0].computeSession).toBeUndefined();
    expect(gateway.backend.mock.calls[0][0].computeConfig).toBeUndefined();
    expect(gateway.constructions).toHaveLength(0);
    expect(gateway.launch).not.toHaveBeenCalled();
    expect(
      (await gateway.post({ ...request('too-much'), gpu: { ...policy, max_launches: 2 } })).status,
    ).toBe(400);
    expect(gateway.backend).toHaveBeenCalledTimes(1);
  });

  it('keeps receipt-pending runs stopping, preserves replay during outages, and emits terminal SSE only after settlement', async () => {
    const gateway = await boot();
    const body = request('pending-smoke');
    const submitted = await gateway.post(body);
    expect(submitted.status).toBe(200);
    const { run_id } = (await submitted.json()) as { run_id: string };
    const pendingState = await untilStatus(gateway, run_id, 'stopping');
    expect(pendingState).toMatchObject({
      compute: { complete: false, pendingJobIds: ['smoke-job'] },
    });
    expect(pendingState.costUsd).toBeUndefined();
    expect(gateway.constructions[0].cfg).toMatchObject({
      maxUsdcMicros: 200000,
      maxDurationSecs: 300,
      maxLaunches: 1,
    });
    expect(gateway.constructions[0].options.signal).toBeInstanceOf(AbortSignal);
    expect(gateway.backend.mock.calls[0][0].maxProviderCostUsd).toBeCloseTo(0.18);

    const persistedPending = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(
      persistedPending.events.some(
        (event: { type: string }) => event.type === 'run.completed' || event.type === 'run.failed',
      ),
    ).toBe(false);
    gateway.setReady(false);
    const replay = await gateway.post(body);
    expect(replay.status).toBe(200);
    expect(await replay.json()).toEqual({ run_id });
    const found = await gateway.get('/v1/run-sessions/pending-smoke');
    expect(found.status).toBe(200);
    expect(await found.json()).toEqual({ run_id });
    expect((await gateway.post(request('new-during-outage'))).status).toBe(503);
    expect(gateway.backend).toHaveBeenCalledTimes(1);
    expect(gateway.launch).toHaveBeenCalledTimes(1);

    const stream = await gateway.get(`/v1/runs/${run_id}/events`);
    const reader = stream.body!.getReader();
    readers.push(reader);
    const first = await reader.read();
    expect(first.done).toBe(false);
    const firstText = new TextDecoder().decode(first.value);
    expect(firstText).toContain('working');
    expect(firstText).not.toContain('run.completed');
    const terminalRead = reader.read();
    gateway.reports.set('pending-smoke', settled('pending-smoke'));
    await gateway.reconcileCompute();
    const terminal = await terminalRead;
    expect(terminal.done).toBe(false);
    expect(new TextDecoder().decode(terminal.value)).toContain('run.completed');
    const final = await untilStatus(gateway, run_id, 'completed');
    expect(final.compute).toEqual(settled('pending-smoke'));
    // 100 input + 10 output tokens at Sonnet prices, $0.12 sandbox ceiling,
    // and the actual $0.01 GPU charge (not the $0.10 reservation or $0.20 cap).
    expect(final.costUsd).toBeCloseTo(0.13045, 6);
    expect(final.output).toContain('"charged_usdc_micros":10000');
    const persistedFinal = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(
      persistedFinal.events.filter((event: { type: string }) => event.type === 'run.completed'),
    ).toHaveLength(1);
    const budget = (await (await gateway.get('/v1/budget')).json()) as Record<string, any>;
    expect(budget.dailyUsd).toBeCloseTo(final.costUsd, 6);
    expect(budget.reserved).toBe(0);
    expect(budget.active).toBe(0);
  });

  it('does not expose a terminal state or event when final receipt persistence fails', async () => {
    const gateway = await boot({ failTerminalPersist: true });
    const response = await gateway.post(request('persist-failure'));
    const { run_id } = (await response.json()) as { run_id: string };
    await untilStatus(gateway, run_id, 'stopping');
    gateway.reports.set('persist-failure', settled('persist-failure'));
    await gateway.reconcileCompute();
    const state = (await (await gateway.get(`/v1/runs/${run_id}`)).json()) as Record<string, any>;
    expect(state.status).toBe('stopping');
    const stream = await gateway.get(`/v1/runs/${run_id}/events`);
    const reader = stream.body!.getReader();
    readers.push(reader);
    const first = await reader.read();
    expect(new TextDecoder().decode(first.value)).not.toContain('run.completed');
    const terminalRead = reader.read();
    await gateway.reconcileCompute();
    expect(new TextDecoder().decode((await terminalRead).value)).toContain('run.completed');
    await untilStatus(gateway, run_id, 'completed');
    const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(
      saved.events.filter((event: { type: string }) => event.type === 'run.completed'),
    ).toHaveLength(1);
    expect(saved.output.match(/gpu_workspace final receipts/g)).toHaveLength(1);
  });

  it('keeps startup recovery fail-closed when saved pending evidence has no journal record', async () => {
    const key = 'missing-journal';
    const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
    const gateway = await boot({
      stored: [
        {
          id: 'crashed-run',
          sessionId: key,
          requestFingerprint: 'a'.repeat(64),
          reservationId: 'reservation-before-crash',
          reservedMax: 0.5,
          status: 'running',
          events: [],
          gpu: policy,
          compute: pending(key),
          updatedAt: new Date().toISOString(),
        },
      ],
    });
    const state = (await (await gateway.get('/v1/runs/crashed-run')).json()) as Record<string, any>;
    expect(state).toMatchObject({
      status: 'stopping',
      compute: { complete: false, pendingJobIds: ['smoke-job'] },
    });
    expect(await (await gateway.get('/v1/capabilities')).json()).toMatchObject({
      features: { gpu_workspace: false },
    });
    expect((await gateway.post(request('after-crash'))).status).toBe(503);
    const saved = JSON.parse(readFileSync(gateway.runPath, 'utf8'))[0];
    expect(
      saved.events.some(
        (event: { type: string }) => event.type === 'run.completed' || event.type === 'run.failed',
      ),
    ).toBe(false);
    expect(gateway.launch).not.toHaveBeenCalled();
    expect(errorLog).toHaveBeenCalledWith(expect.stringContaining('journal record is missing'));
  });
});
