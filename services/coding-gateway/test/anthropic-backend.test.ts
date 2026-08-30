import { afterEach, describe, expect, it, vi } from 'vitest';
import type Anthropic from '@anthropic-ai/sdk';
import {
  computePreview,
  execTool,
  gpuWorkspaceTool,
  previewOf,
  reapNote,
  toolsFor,
  withTurnCache,
} from '../src/backends/anthropic.js';
import { ComputeSession, type ComputeConfig } from '../src/compute.js';
import { isolatedShellCommand } from '../src/sandbox-command.js';
import type { GatewayEvent, Sandbox } from '../src/types.js';

// The Anthropic backend's run() loop builds its own client with no injection
// seam, so it can't be driven without a live key. The novel logic lives in
// pure / injectable helpers: withTurnCache (prompt-cache breakpoint strategy,
// pure over a message array) and execTool (tool dispatch, takes an injected
// sandbox so it is mock-drivable with no key). Both are pinned directly.

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
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      const collection = String(url).endsWith('/v1/jobs');
      const body = String(url).endsWith('/v1/offers')
        ? [
            {
              id: 'a',
              gpu: { model: 'RTX 4090', vram_mib: 49_140 },
              rate_usdc_micros_per_hour: 100_000,
              online: true,
            },
          ]
        : {
            id: collection ? `j${++launched}` : String(url).split('/').pop(),
            status: 'provisioning',
            offer_id: 'vast:1:1',
            maximum_usdc_micros: 50_000,
            access_url: null,
            error: null,
            receipt: null,
            ...job,
          };
      return new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  return calls;
}

afterEach(() => vi.unstubAllGlobals());

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

  it('tells the model to wait between status polls', () => {
    // Seven back-to-back polls in 25s, each a full model turn re-sending the
    // whole transcript, on a provider that takes minutes to provision.
    expect(gpuWorkspaceTool(COMPUTE_CFG).description).toMatch(/sleep 25/);
  });

  it('tells the model it cannot reach the workspace and that the URL is a credential', () => {
    // Sandbox egress is restricted to package registries, so an agent that
    // believes it can drive the GPU itself spends real USDC on nothing.
    const description = gpuWorkspaceTool(COMPUTE_CFG).description!;
    expect(description).toMatch(/cannot use the workspace yourself/);
    expect(description).toMatch(/package registries/);
    expect(description).toMatch(/live credential/);
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
      offer_id: 'vast:1:1',
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

  it('returns the access URL, error and receipt on status', async () => {
    mockMarket({ status: 'running', access_url: 'https://gpu.test/lab?token=secret' });
    const compute = new ComputeSession(COMPUTE_CFG);
    await execTool(toolUse('gpu_workspace', { action: 'launch' }), memSandbox(), () => {}, compute);
    const out = await execTool(
      toolUse('gpu_workspace', { action: 'status', job_id: 'j1' }),
      memSandbox(),
      () => {},
      compute,
    );
    expect(JSON.parse(out)).toMatchObject({
      job_id: 'j1',
      status: 'running',
      access_url: 'https://gpu.test/lab?token=secret',
    });
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
    await expect(call).rejects.toThrow(/must be launch, status, or cancel/);
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
  it('is empty when the gateway has no compute session', async () => {
    expect(await reapNote(null)).toBe('');
  });

  it('is empty when the run launched nothing', async () => {
    expect(await reapNote(new ComputeSession(COMPUTE_CFG))).toBe('');
  });

  it('names every workspace cancelled at run end', async () => {
    mockMarket({ status: 'cancelled' });
    const compute = new ComputeSession(COMPUTE_CFG);
    await compute.launch();
    await compute.launch();
    expect(await reapNote(compute)).toBe('\n[gpu_workspace cancelled at run end: j1, j2]');
    expect(await reapNote(compute)).toBe('');
  });
});
