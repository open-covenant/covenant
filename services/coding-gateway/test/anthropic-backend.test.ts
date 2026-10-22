import { afterEach, describe, expect, it, vi } from 'vitest';
import type Anthropic from '@anthropic-ai/sdk';
import {
  AnthropicBackend,
  computePreview,
  execTool,
  gpuWorkspaceTool,
  previewOf,
  reapNote,
  toolsFor,
  withTurnCache,
} from '../src/backends/anthropic.js';
import { ComputeSession, type ComputeConfig, type ComputeCleanupReport } from '../src/compute.js';
import { isolatedShellCommand } from '../src/sandbox-command.js';
import { config, PRICING } from '../src/config.js';
import type { CodingBackend, GatewayEvent, Sandbox } from '../src/types.js';

// All provider and control-plane calls below use injected fakes; no live requests.

function memSandbox(
  files: Record<string, string> = {},
  execResult: { stdout: string; stderr: string; exitCode: number } = {
    stdout: '',
    stderr: '',
    exitCode: 0,
  },
): Sandbox & { execs: string[] } {
  const store = { ...files };
  const execs: string[] = [];
  return {
    readFile: async (p) => {
      if (!(p in store)) throw new Error(`ENOENT: ${p}`);
      return store[p]!;
    },
    writeFile: async (p, c) => {
      store[p] = c;
    },
    exec: async (cmd) => {
      execs.push(cmd);
      return execResult;
    },
    previewUrl: async () => '',
    destroy: async () => {},
    execs,
  } as Sandbox & { execs: string[] };
}

function toolUse(name: string, input: Record<string, unknown>): Anthropic.ToolUseBlock {
  return { id: 'toolu_1', name, input } as Anthropic.ToolUseBlock;
}

type Block = Anthropic.ContentBlockParam;

const COMPUTE_CFG: ComputeConfig = {
  apiUrl: 'https://compute.test',
  apiToken: 'beta-token',
  maxUsdcMicros: 200_000,
  maxDurationSecs: 1_800,
  maxLaunches: 4,
};

/** A control plane with one cheap offer, jobs named j1, j2, ... in order. */
function mockMarket(job: Record<string, unknown> = {}) {
  let launched = 0;
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  const jobs = new Map<string, Record<string, unknown>>();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      if (String(url).endsWith('/v1/offers')) {
        return new Response(
          JSON.stringify([
            {
              id: 'a',
              gpu: { model: 'RTX 4090', vram_mib: 49_140 },
              rate_usdc_micros_per_hour: 100_000,
              online: true,
            },
          ]),
          { status: 200 },
        );
      }
      let current: Record<string, unknown>;
      if (init?.method === 'POST') {
        const plan = JSON.parse(String(init.body));
        current = {
          id: `j${++launched}`,
          status: 'provisioning',
          offer_id: plan.offer.id,
          maximum_usdc_micros: plan.maximum_usdc_micros,
          access_url: null,
          error: null,
          receipt: null,
          ...job,
        };
        jobs.set(String(current.id), current);
      } else {
        current = jobs.get(String(url).split('/').pop()!)!;
        if (init?.method === 'DELETE') current.status = 'cancelled';
      }
      if (
        ['cancelled', 'completed', 'failed'].includes(String(current.status)) &&
        !current.receipt
      ) {
        current.receipt = {
          runtime_secs: 0,
          charged_usdc_micros: 0,
          refunded_usdc_micros: current.maximum_usdc_micros,
        };
      }
      return new Response(JSON.stringify(current), { status: 200 });
    }),
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

describe('withTurnCache', () => {
  it('marks only the last block of the last message and leaves the input untouched', () => {
    const messages = [
      { role: 'user', content: [{ type: 'text', text: 'first' }] },
      { role: 'assistant', content: [{ type: 'text', text: 'second' }] },
    ] as Anthropic.MessageParam[];

    const out = withTurnCache(messages);

    expect((out[1]!.content as Block[])[0]).toMatchObject({ cache_control: { type: 'ephemeral' } });
    expect(
      ((out[0]!.content as Block[])[0] as { cache_control?: unknown }).cache_control,
    ).toBeUndefined();
    // The input array is neither replaced nor mutated: cloning the block keeps
    // the breakpoint off the shared reference so it can't accumulate across turns.
    expect(out).not.toBe(messages);
    expect(
      ((messages[1]!.content as Block[])[0] as { cache_control?: unknown }).cache_control,
    ).toBeUndefined();
  });

  it('converts string message content into a cached text block', () => {
    const messages = [{ role: 'user', content: 'hello' }] as Anthropic.MessageParam[];

    const out = withTurnCache(messages);

    expect((out[0]!.content as Block[])[0]).toMatchObject({
      type: 'text',
      text: 'hello',
      cache_control: { type: 'ephemeral' },
    });
  });

  it('passes an empty message list through without throwing', () => {
    expect(withTurnCache([])).toEqual([]);
  });
});

describe('execTool', () => {
  it('throws when edit_file old_string is not found', async () => {
    const sandbox = memSandbox({ 'a.txt': 'hello world' });
    const call = execTool(
      toolUse('edit_file', { path: 'a.txt', old_string: 'missing', new_string: 'x' }),
      sandbox,
      () => {},
    );
    await expect(call).rejects.toThrow(/not found in a\.txt/);
  });

  it('throws when edit_file old_string is not unique', async () => {
    const sandbox = memSandbox({ 'a.txt': 'x x' });
    const call = execTool(
      toolUse('edit_file', { path: 'a.txt', old_string: 'x', new_string: 'y' }),
      sandbox,
      () => {},
    );
    await expect(call).rejects.toThrow(/not unique in a\.txt/);
  });

  it('throws for an unknown tool', async () => {
    const call = execTool(toolUse('delete_file', { path: 'a.txt' }), memSandbox(), () => {});
    await expect(call).rejects.toThrow(/unknown tool: delete_file/);
  });

  it('runs a bash tool call through sandbox.exec and formats exit/stdout/stderr', async () => {
    // bash is the agent's command tool and only its error arms are pinned
    // elsewhere, so a regression here falls through to the unknown-tool default.
    const sandbox = memSandbox({}, { stdout: 'file.txt', stderr: 'warn', exitCode: 7 });
    const out = await execTool(toolUse('bash', { command: 'ls' }), sandbox, () => {});
    expect(sandbox.execs).toContain(isolatedShellCommand('ls'));
    expect(out).toBe('exit=7\n--- stdout ---\nfile.txt\n--- stderr ---\nwarn');
  });

  it('splices old_string for new_string once on a successful edit_file', async () => {
    // Only the not-found and not-unique arms are pinned above, so splice math
    // that drifts would corrupt the file with every other test still green.
    const sandbox = memSandbox({ 'a.txt': 'xx foo yy' });
    const emitted: GatewayEvent[] = [];
    const out = await execTool(
      toolUse('edit_file', { path: 'a.txt', old_string: 'foo', new_string: 'bar' }),
      sandbox,
      (e) => emitted.push(e),
    );
    expect(out).toBe('edited a.txt');
    expect(await sandbox.readFile('a.txt')).toBe('xx bar yy');
    expect(emitted.find((e) => e.type === 'file.written')).toMatchObject({
      path: 'a.txt',
      bytes: 9,
    });
  });

  it('writes a file via write_file and reports byte length, not char length', async () => {
    // file.written must report byte length: "héllo" is 5 characters and 6 UTF-8
    // bytes, so a char-count regression stays invisible on ASCII content.
    const content = 'héllo';
    const sandbox = memSandbox();
    const emitted: GatewayEvent[] = [];
    const out = await execTool(toolUse('write_file', { path: 'a.txt', content }), sandbox, (e) =>
      emitted.push(e),
    );
    expect(await sandbox.readFile('a.txt')).toBe(content);
    expect(out).toBe(`wrote ${Buffer.byteLength(content)} bytes to a.txt`);
    expect(emitted.find((e) => e.type === 'file.written')).toMatchObject({
      path: 'a.txt',
      bytes: Buffer.byteLength(content),
    });
  });
});

describe('previewOf', () => {
  it('truncates a bash command to 120 chars for the event preview', () => {
    const preview = previewOf(toolUse('bash', { command: 'x'.repeat(200) }));
    expect(preview).toHaveLength(120);
    expect(preview).toBe('x'.repeat(120));
  });

  it('returns the path for a path-bearing tool', () => {
    expect(previewOf(toolUse('read_file', { path: 'src/app.ts' }))).toBe('src/app.ts');
  });

  it('returns empty for a tool with neither a command nor a path', () => {
    expect(previewOf(toolUse('custom', {}))).toBe('');
  });
});

describe('gpu_workspace tool gating', () => {
  it('advertises the GPU tool only when compute is configured', () => {
    expect(toolsFor(null).map((t) => t.name)).not.toContain('gpu_workspace');
    expect(toolsFor(COMPUTE_CFG).map((t) => t.name)).toContain('gpu_workspace');
    expect(toolsFor(COMPUTE_CFG)).toHaveLength(toolsFor(null).length + 1);
  });

  it('states the run budget, the launch cap and the booking window', () => {
    const description = gpuWorkspaceTool(COMPUTE_CFG).description!;
    expect(description).toContain('4 GPU workspaces');
    expect(description).toContain('$0.20');
    expect(description).toContain('default 1800s');
    expect(description).toContain('maximum 1800s');
    expect(gpuWorkspaceTool({ ...COMPUTE_CFG, maxLaunches: 1 }).description).toContain(
      '1 GPU workspace ',
    );
  });

  it('tells the model to run right after launch instead of polling', () => {
    // Every poll is a full model turn re-sending the whole transcript, on a
    // provider that takes minutes to provision; run waits server-side instead.
    const description = gpuWorkspaceTool(COMPUTE_CFG).description!;
    expect(description).toMatch(/call it right after launch instead of polling status/);
    expect(description).not.toMatch(/access_url|access URL/);
  });

  it('describes the run contract the gateway enforces', () => {
    const tool = gpuWorkspaceTool(COMPUTE_CFG);
    expect(tool.input_schema.properties).toMatchObject({
      action: { enum: ['launch', 'run', 'status', 'cancel'] },
      command: { type: 'string' },
      files: { type: 'array' },
      outputs: { type: 'array' },
    });
    expect(tool.description).toMatch(/download link in the final result/);
  });
});

describe('computePreview', () => {
  it('records what was booked, so audit does not depend on the model narrating it', () => {
    const result = JSON.stringify({
      job_id: 'j1',
      status: 'provisioning',
      offer_id: 'vast:46151930:29558',
      maximum_usdc_micros: 188_593,
    });
    expect(computePreview({ action: 'launch' }, result, false)).toBe(
      'launch job=j1 status=provisioning offer=vast:46151930:29558 booked_max_usdc_micros=188593',
    );
  });

  it('records what a cancel charged and refunded', () => {
    const result = JSON.stringify({
      job_id: 'j1',
      status: 'cancelled',
      access_url: null,
      error: null,
      receipt: { runtime_secs: 120, charged_usdc_micros: 12_573, refunded_usdc_micros: 176_020 },
    });
    expect(computePreview({ action: 'cancel' }, result, false)).toBe(
      'cancel job=j1 status=cancelled charged_usdc_micros=12573 refunded_usdc_micros=176020',
    );
  });

  it('never carries the access URL, which is a live credential', () => {
    const result = JSON.stringify({
      job_id: 'j1',
      status: 'running',
      access_url: 'https://gpu.test/lab?token=secret',
      error: null,
      receipt: null,
    });
    const preview = computePreview({ action: 'status' }, result, false);
    expect(preview).toBe('status job=j1 status=running');
    expect(preview).not.toContain('secret');
  });

  it('records the refusal when a call fails', () => {
    expect(
      computePreview(
        { action: 'launch' },
        'error: launch cap reached: this run may launch 1 GPU workspace',
        true,
      ),
    ).toBe('launch failed: launch cap reached: this run may launch 1 GPU workspace');
  });
});

describe('execTool gpu_workspace', () => {
  it('refuses when the gateway has no compute configuration', async () => {
    const call = execTool(toolUse('gpu_workspace', { action: 'launch' }), memSandbox(), () => {});
    await expect(call).rejects.toThrow(/not enabled on this gateway/);
  });

  it('launches and reports the job identity and its committed maximum', async () => {
    mockMarket();
    const out = await execTool(
      toolUse('gpu_workspace', { action: 'launch' }),
      memSandbox(),
      () => {},
      new ComputeSession(COMPUTE_CFG),
    );
    expect(JSON.parse(out)).toEqual({
      job_id: 'j1',
      status: 'provisioning',
      offer_id: 'a',
      maximum_usdc_micros: 50_000,
    });
  });

  it('reads a duration the model sent as a string instead of booking the maximum', async () => {
    const calls = mockMarket();
    await execTool(
      toolUse('gpu_workspace', { action: 'launch', duration_secs: '600' }),
      memSandbox(),
      () => {},
      new ComputeSession(COMPUTE_CFG),
    );
    const post = calls.find((c) => c.init?.method === 'POST')!;
    expect(JSON.parse(String(post.init!.body)).duration_secs).toBe(600);
  });

  it('reports readiness without the workspace address on status', async () => {
    mockMarket({ status: 'running', access_url: 'https://gpu.test/lab?token=secret' });
    const compute = new ComputeSession(COMPUTE_CFG);
    await execTool(toolUse('gpu_workspace', { action: 'launch' }), memSandbox(), () => {}, compute);
    const out = await execTool(
      toolUse('gpu_workspace', { action: 'status', job_id: 'j1' }),
      memSandbox(),
      () => {},
      compute,
    );
    expect(JSON.parse(out)).toMatchObject({ job_id: 'j1', status: 'running', ready: true });
    expect(out).not.toContain('secret');
  });

  it('refuses run when the gateway supplied no runner', async () => {
    const call = execTool(
      toolUse('gpu_workspace', { action: 'run', job_id: 'j1', command: 'nvidia-smi' }),
      memSandbox(),
      () => {},
      new ComputeSession(COMPUTE_CFG),
    );
    await expect(call).rejects.toThrow(/not enabled/);
  });

  it('refuses a job id that walks out of the jobs route', async () => {
    const calls = mockMarket();
    const compute = new ComputeSession(COMPUTE_CFG);
    await execTool(toolUse('gpu_workspace', { action: 'launch' }), memSandbox(), () => {}, compute);
    const call = execTool(
      toolUse('gpu_workspace', { action: 'status', job_id: '../../v1/admin/tokens' }),
      memSandbox(),
      () => {},
      compute,
    );
    await expect(call).rejects.toThrow(/not a job id/);
    expect(calls.some((c) => c.url.includes('admin'))).toBe(false);
  });

  it('rejects an action outside the schema enum', async () => {
    const call = execTool(
      toolUse('gpu_workspace', { action: 'destroy', job_id: 'j1' }),
      memSandbox(),
      () => {},
      new ComputeSession(COMPUTE_CFG),
    );
    await expect(call).rejects.toThrow(/must be launch, run, status, or cancel/);
  });

  it('requires a job id for status and cancel', async () => {
    const call = execTool(
      toolUse('gpu_workspace', { action: 'cancel' }),
      memSandbox(),
      () => {},
      new ComputeSession(COMPUTE_CFG),
    );
    await expect(call).rejects.toThrow(/job_id is required/);
  });
});

describe('reapNote', () => {
  it('is empty without a report or when the run launched nothing', () => {
    expect(reapNote(null)).toBe('');
    expect(
      reapNote({
        runKey: 'run',
        settled: [],
        pendingJobIds: [],
        pendingLaunchKeys: [],
        complete: true,
      }),
    ).toBe('');
  });

  it('formats only confirmed settlements and distinguishes pending cleanup', () => {
    const report: ComputeCleanupReport = {
      runKey: 'run',
      settled: [
        {
          job_id: 'j1',
          status: 'cancelled',
          maximum_usdc_micros: 50_000,
          receipt: { runtime_secs: 0, charged_usdc_micros: 0, refunded_usdc_micros: 50_000 },
        },
      ],
      pendingJobIds: [],
      pendingLaunchKeys: [],
      complete: true,
    };
    expect(reapNote(report)).toBe('\n[gpu_workspace settled at run end: j1]');
    expect(reapNote({ ...report, complete: false, pendingJobIds: ['j2'] })).toBe(
      '\n[gpu_workspace cleanup pending; final receipts are not confirmed]',
    );
  });
});

type RunOptions = Parameters<CodingBackend['run']>[0];
function runOptions(extra: Partial<RunOptions> = {}): RunOptions {
  return {
    input: 'do a small task',
    sandbox: memSandbox(),
    signal: new AbortController().signal,
    emit: () => {},
    maxProviderCostUsd: 0.18,
    ...extra,
  };
}
function modelMessage(extra: Partial<Anthropic.Message> = {}): Anthropic.Message {
  return {
    id: 'msg-1',
    type: 'message',
    role: 'assistant',
    model: 'claude-sonnet-4-6',
    content: [{ type: 'text', text: 'done', citations: null }],
    stop_reason: 'end_turn',
    stop_sequence: null,
    usage: {
      input_tokens: 100,
      output_tokens: 10,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    },
    ...extra,
  } as Anthropic.Message;
}
function fakeClient(responses: Anthropic.Message[] = [modelMessage()], inputTokens = 100) {
  let next = 0;
  const countTokens = vi.fn(async () => ({ input_tokens: inputTokens }));
  const stream = vi.fn((_request: Anthropic.MessageStreamParams, _options?: unknown) => {
    const message = responses[Math.min(next++, responses.length - 1)]!;
    return {
      async *[Symbol.asyncIterator]() {},
      finalMessage: async () => message,
    };
  });
  const client = { messages: { countTokens, stream } } as unknown as Pick<Anthropic, 'messages'>;
  return { client, countTokens, stream };
}

describe('Anthropic per-run consent and accounting', () => {
  it('does not expose GPU tools from a global token alone', async () => {
    vi.stubEnv('COMPUTE_API_TOKEN', 'test-token-not-live');
    vi.resetModules();
    const { AnthropicBackend: FreshBackend } = await import('../src/backends/anthropic.js');
    const fake = fakeClient();
    await new FreshBackend(undefined, fake.client).run(runOptions());
    expect(fake.stream.mock.calls[0]![0].tools!.map((tool) => tool.name)).not.toContain(
      'gpu_workspace',
    );
  });

  it('uses only the supplied GPU session and leaves cleanup, charges and terminal events to the server', async () => {
    const compute = new ComputeSession(COMPUTE_CFG);
    const launch = vi.spyOn(compute, 'launch').mockResolvedValue({
      id: 'j1',
      status: 'provisioning',
      offer_id: 'a',
      maximum_usdc_micros: 50_000,
      access_url: null,
      error: null,
      receipt: null,
    });
    const reap = vi.spyOn(compute, 'reap');
    const fake = fakeClient([
      modelMessage({
        content: [
          {
            type: 'tool_use',
            id: 'gpu-call',
            name: 'gpu_workspace',
            input: { action: 'launch' },
          } as Anthropic.ToolUseBlock,
        ],
        stop_reason: 'tool_use',
      }),
      modelMessage(),
    ]);
    const events: GatewayEvent[] = [];
    const charge = vi.fn();
    const record = vi.fn();
    const result = await new AnthropicBackend(undefined, fake.client).run(
      runOptions({
        computeSession: compute,
        computeConfig: COMPUTE_CFG,
        recordComputeUsd: charge,
        recordProviderRequest: record,
        emit: (event) => events.push(event),
      }),
    );
    expect(result.output).toBe('done');
    expect(launch).toHaveBeenCalledOnce();
    expect(fake.stream.mock.calls[0]![0].tools!.map((tool) => tool.name)).toContain(
      'gpu_workspace',
    );
    expect(reap).not.toHaveBeenCalled();
    expect(charge).not.toHaveBeenCalled();
    expect(events.some((event) => event.type.startsWith('run.'))).toBe(false);
    expect(record).toHaveBeenCalledTimes(2);
  });

  it('rejects partial GPU injection before any provider request', async () => {
    const fake = fakeClient();
    const backend = new AnthropicBackend(undefined, fake.client);
    await expect(backend.run(runOptions({ computeConfig: COMPUTE_CFG }))).rejects.toThrow(
      /supplied together/,
    );
    await expect(
      backend.run(runOptions({ computeSession: new ComputeSession(COMPUTE_CFG) })),
    ).rejects.toThrow(/supplied together/);
    expect(fake.countTokens).not.toHaveBeenCalled();
    expect(fake.stream).not.toHaveBeenCalled();
  });

  it('does not start a GPU tool when cancellation arrives with the final model response', async () => {
    const ctrl = new AbortController();
    const compute = new ComputeSession(COMPUTE_CFG);
    const launch = vi.spyOn(compute, 'launch');
    const fake = fakeClient();
    fake.stream.mockImplementationOnce(() => ({
      async *[Symbol.asyncIterator]() {},
      finalMessage: async () => {
        ctrl.abort();
        return modelMessage({
          content: [
            {
              type: 'tool_use',
              id: 'gpu-call',
              name: 'gpu_workspace',
              input: { action: 'launch' },
            } as Anthropic.ToolUseBlock,
          ],
          stop_reason: 'tool_use',
        });
      },
    }));
    await expect(
      new AnthropicBackend(undefined, fake.client).run(
        runOptions({ signal: ctrl.signal, computeSession: compute, computeConfig: COMPUTE_CFG }),
      ),
    ).rejects.toThrow(/run aborted/);
    expect(launch).not.toHaveBeenCalled();
  });

  it('checks cancellation between tools and never converts it into a recoverable tool error', async () => {
    const ctrl = new AbortController();
    const compute = new ComputeSession(COMPUTE_CFG);
    const launch = vi.spyOn(compute, 'launch');
    const sandbox = memSandbox();
    sandbox.exec = async () => {
      ctrl.abort();
      return { stdout: '', stderr: '', exitCode: 0 };
    };
    const fake = fakeClient([
      modelMessage({
        content: [
          {
            type: 'tool_use',
            id: 'shell-call',
            name: 'bash',
            input: { command: 'true' },
          } as Anthropic.ToolUseBlock,
          {
            type: 'tool_use',
            id: 'gpu-call',
            name: 'gpu_workspace',
            input: { action: 'launch' },
          } as Anthropic.ToolUseBlock,
        ],
        stop_reason: 'tool_use',
      }),
    ]);
    await expect(
      new AnthropicBackend(undefined, fake.client).run(
        runOptions({
          sandbox,
          signal: ctrl.signal,
          computeSession: compute,
          computeConfig: COMPUTE_CFG,
        }),
      ),
    ).rejects.toThrow(/run aborted/);
    expect(launch).not.toHaveBeenCalled();
    expect(fake.stream).toHaveBeenCalledOnce();
  });

  it('counts the exact prompt and bounds output at the worst cache-write price before generation', async () => {
    const fake = fakeClient([modelMessage()], 1_000);
    await new AnthropicBackend(undefined, fake.client).run(
      runOptions({ maxProviderCostUsd: 0.02 }),
    );
    const request = fake.stream.mock.calls[0]![0];
    const counted = fake.countTokens.mock.calls[0] as unknown as [
      Anthropic.MessageCountTokensParams,
      unknown,
    ];
    expect(counted[0]).toMatchObject({
      model: request.model,
      system: request.system,
      messages: request.messages,
      tools: request.tools,
      thinking: request.thinking,
    });
    const price = PRICING[config.model]!;
    const inputReserve = Math.ceil(
      (Math.ceil(1_000 * 1.1) + 1_024) * Math.max(price.input, price.cacheWrite, price.cacheRead),
    );
    expect(inputReserve + request.max_tokens * price.output).toBeLessThanOrEqual(20_000);
    expect(request.max_tokens).toBeLessThan(1_000);
    expect(fake.stream.mock.calls[0]![1]).toMatchObject({ maxRetries: 0 });
    expect(counted[1]).toMatchObject({ maxRetries: 0 });
  });

  it('refuses unaffordable input and invalid token counts before generation', async () => {
    for (const count of [1_000_000, NaN, -1, 1.2]) {
      const fake = fakeClient([modelMessage()], count);
      await expect(
        new AnthropicBackend(undefined, fake.client).run(runOptions()),
      ).rejects.toThrow();
      expect(fake.stream).not.toHaveBeenCalled();
    }
  });

  it('recalculates the next turn from observed usage and stops before another unaffordable request', async () => {
    const fake = fakeClient(
      [
        modelMessage({
          stop_reason: 'pause_turn',
          usage: {
            input_tokens: 1_000,
            output_tokens: 1_100,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          } as Anthropic.Usage,
        }),
      ],
      100,
    );
    const record = vi.fn();
    await expect(
      new AnthropicBackend(undefined, fake.client).run(
        runOptions({ maxProviderCostUsd: 0.02, recordProviderRequest: record }),
      ),
    ).rejects.toThrow(/budget exhausted before generation/);
    expect(fake.stream).toHaveBeenCalledOnce();
    expect(record).toHaveBeenCalledOnce();
  });

  it('fails closed for unknown model prices', async () => {
    const saved = config.model;
    (config as { model: string }).model = 'unknown-model';
    try {
      const fake = fakeClient();
      await expect(new AnthropicBackend(undefined, fake.client).run(runOptions())).rejects.toThrow(
        /recognized/,
      );
      expect(fake.countTokens).not.toHaveBeenCalled();
      expect(fake.stream).not.toHaveBeenCalled();
    } finally {
      (config as { model: string }).model = saved;
    }
  });

  it('throws at output and turn exhaustion without a success terminal event', async () => {
    const events: GatewayEvent[] = [];
    const truncated = fakeClient([modelMessage({ stop_reason: 'max_tokens' })]);
    await expect(
      new AnthropicBackend(undefined, truncated.client).run(
        runOptions({ emit: (event) => events.push(event) }),
      ),
    ).rejects.toThrow(/output token budget/);
    const endless = fakeClient([modelMessage({ stop_reason: 'pause_turn' })]);
    const record = vi.fn();
    await expect(
      new AnthropicBackend(undefined, endless.client).run(
        runOptions({
          maxProviderCostUsd: 10,
          recordProviderRequest: record,
          emit: (event) => events.push(event),
        }),
      ),
    ).rejects.toMatchObject({
      message: 'exceeded 30 Anthropic turns',
      usage: { inputTokens: 3_000, outputTokens: 300 },
    });
    expect(endless.stream).toHaveBeenCalledTimes(30);
    expect(record).toHaveBeenCalledTimes(30);
    expect(events.some((event) => event.type === 'run.completed')).toBe(false);
  });

  it('does not secretly retry a failed SDK generation request', async () => {
    const fetcher = vi.fn(async (url: RequestInfo | URL) => {
      if (String(url).includes('count_tokens'))
        return new Response(JSON.stringify({ input_tokens: 100 }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      return new Response(
        JSON.stringify({
          type: 'error',
          error: { type: 'overloaded_error', message: 'test overload' },
        }),
        { status: 529, headers: { 'content-type': 'application/json' } },
      );
    });
    vi.stubGlobal('fetch', fetcher);
    const record = vi.fn();
    await expect(
      new AnthropicBackend('test-key-not-live').run(runOptions({ recordProviderRequest: record })),
    ).rejects.toThrow();
    expect(
      fetcher.mock.calls.filter(([url]) => !String(url).includes('count_tokens')),
    ).toHaveLength(1);
    expect(record).toHaveBeenCalledOnce();
  });
});
