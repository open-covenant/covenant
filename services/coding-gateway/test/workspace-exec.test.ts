import { describe, expect, it, vi } from 'vitest';
import {
  MAX_OUTPUT_BYTES,
  WorkspaceClient,
  WorkspaceError,
  parseWorkspaceUrl,
  runWorkspaceJob,
  safeRelativePath,
  type KernelResult,
} from '../src/workspace-exec.js';

const result = (body: unknown): KernelResult => ({
  ok: true,
  stdout: `noise\n__covenant__${JSON.stringify(body)}\n`,
});

/** In-memory Jupyter: files by path, and kernels that understand the three control snippets. */
function fakeWorkspace(options: {
  exitAfterPolls?: number;
  exit?: { exit_code: number; duration_ms: number };
  produce?: Record<string, Buffer>;
  sizes?: Record<string, number | null>;
}) {
  const files = new Map<string, Buffer>();
  const directories = new Set<string>();
  let polls = 0;
  const executed: string[] = [];
  const client = {
    waitUntilServing: vi.fn(async () => {}),
    makeDirectory: vi.fn(async (path: string) => {
      directories.add(path);
    }),
    upload: vi.fn(async (path: string, bytes: Buffer) => {
      files.set(path, bytes);
    }),
    download: vi.fn(async (path: string) => {
      if (path.endsWith('/covenant-exit.json')) {
        polls++;
        if (options.exitAfterPolls === undefined || polls < options.exitAfterPolls) return null;
        return Buffer.from(JSON.stringify(options.exit ?? { exit_code: 0, duration_ms: 1234 }));
      }
      const name = path.split('/').slice(2).join('/');
      return options.produce?.[name] ?? null;
    }),
    execute: vi.fn(async (code: string) => {
      if (code.includes('covenant-run.py')) {
        executed.push('start');
        return result({ pid: 4242 });
      }
      if (code.includes('killpg')) {
        executed.push('kill');
        return result({});
      }
      executed.push('collect');
      const sizes =
        options.sizes ??
        Object.fromEntries(
          Object.entries(options.produce ?? {}).map(([name, bytes]) => [name, bytes.length]),
        );
      return result({ stdout: 'out tail', stderr: 'err tail', sizes });
    }),
  };
  return { client, files, directories, executed };
}

describe('workspace address', () => {
  it('accepts the Jupyter address Compute returns', () => {
    expect(parseWorkspaceUrl(`https://203.0.113.7:41234/lab?token=${'a'.repeat(32)}`)).toEqual({
      origin: 'https://203.0.113.7:41234',
      token: 'a'.repeat(32),
    });
    expect(parseWorkspaceUrl(`https://[2001:db8::1]:8080/lab?token=${'b'.repeat(16)}`).origin).toBe(
      'https://[2001:db8::1]:8080',
    );
  });

  it.each([
    `http://203.0.113.7:41234/lab?token=${'a'.repeat(32)}`,
    `https://gpu.example:41234/lab?token=${'a'.repeat(32)}`,
    `https://203.0.113.7/lab?token=${'a'.repeat(32)}`,
    'https://203.0.113.7:41234/lab?token=short',
    `https://user:pass@203.0.113.7:41234/lab?token=${'a'.repeat(32)}`,
    'not a url',
  ])('rejects %s', (raw) => {
    expect(() => parseWorkspaceUrl(raw)).toThrow(WorkspaceError);
  });
});

describe('job paths', () => {
  it.each(['embed.py', 'input/lines.txt', 'out/embeddings.npy'])('accepts %s', (path) => {
    expect(safeRelativePath(path, 'output')).toBe(path);
  });

  it.each([
    '../etc/passwd',
    '/etc/passwd',
    '.env',
    'out/.hidden',
    'a//b',
    'covenant-exit.json',
    'name with spaces',
    '',
    'x'.repeat(129),
    42,
  ])('rejects %j', (path) => {
    expect(() => safeRelativePath(path, 'output')).toThrow(WorkspaceError);
  });
});

describe('runWorkspaceJob', () => {
  it('uploads, starts detached, polls for completion and brings outputs back', async () => {
    const embeddings = Buffer.alloc(3_000, 7);
    const workspace = fakeWorkspace({
      exitAfterPolls: 3,
      produce: { 'out/embeddings.npy': embeddings },
    });
    const outcome = await runWorkspaceJob(
      workspace.client,
      {
        command: 'python3 embed.py',
        files: [{ path: 'input/lines.txt', bytes: Buffer.from('a\nb\n') }],
        outputs: ['out/embeddings.npy', 'report.json'],
        deadlineMs: Date.now() + 10_000,
      },
      { pollMs: 1 },
    );
    expect(outcome).toMatchObject({
      exitCode: 0,
      timedOut: false,
      durationMs: 1234,
      stdoutTail: 'out tail',
      stderrTail: 'err tail',
      missing: ['report.json'],
      skipped: [],
    });
    expect(outcome.outputs).toEqual([{ path: 'out/embeddings.npy', bytes: embeddings }]);
    expect(workspace.executed).toEqual(['start', 'collect']);
    const uploaded = [...workspace.files.keys()].map((path) => path.split('/').slice(2).join('/'));
    expect(uploaded).toEqual(['input/lines.txt', 'covenant-command.sh', 'covenant-run.py']);
    // Parent directories of nested files exist before the upload.
    expect([...workspace.directories].some((dir) => dir.endsWith('/input'))).toBe(true);
    expect(String(workspace.files.get([...workspace.files.keys()][1]!))).toBe('python3 embed.py\n');
  });

  it('stops the whole process group at the deadline and reports a timeout', async () => {
    const workspace = fakeWorkspace({});
    const outcome = await runWorkspaceJob(
      workspace.client,
      { command: 'sleep 999', files: [], outputs: [], deadlineMs: Date.now() + 20 },
      { pollMs: 5 },
    );
    expect(outcome).toMatchObject({ exitCode: null, timedOut: true });
    expect(workspace.executed).toEqual(['start', 'kill', 'collect']);
  });

  it('separates workspace readiness, staging, command and collection time', async () => {
    let clock = 0;
    const workspace = fakeWorkspace({ exitAfterPolls: 1, produce: { 'report.json': Buffer.from('{}') } });
    workspace.client.waitUntilServing.mockImplementation(async () => { clock += 100; });
    workspace.client.makeDirectory.mockImplementation(async () => { clock += 25; });
    workspace.client.upload.mockImplementation(async () => { clock += 40; });
    const execute = workspace.client.execute.getMockImplementation()!;
    workspace.client.execute.mockImplementation(async (code) => {
      clock += code.includes('covenant-run.py') ? 10 : 30;
      return execute(code);
    });
    const download = workspace.client.download.getMockImplementation()!;
    workspace.client.download.mockImplementation(async (path) => {
      clock += path.endsWith('covenant-exit.json') ? 50 : 20;
      return download(path);
    });
    const outcome = await runWorkspaceJob(workspace.client, {
      command: 'python3 embed.py', files: [], outputs: ['report.json'], deadlineMs: 1_000,
    }, { now: () => clock, pollMs: 1 });
    expect(outcome.timings).toEqual({
      workspaceReadyWaitMs: 100, stagingMs: 130, commandBudgetMs: 770,
      commandElapsedMs: 60, collectionMs: 50,
    });
    // Process-reported duration is a separate observation, not the tool's wall time.
    expect(outcome.durationMs).toBe(1234);
  });

  it('does not start a command when staging consumed the remaining booking window', async () => {
    let clock = 0;
    const workspace = fakeWorkspace({});
    workspace.client.upload.mockImplementation(async () => { clock += 60; });
    await expect(runWorkspaceJob(workspace.client, {
      command: 'python3 embed.py', files: [], outputs: [], deadlineMs: 100,
    }, { now: () => clock })).rejects.toThrow(/deadline elapsed before execution/);
    expect(workspace.executed).toEqual([]);
  });

  it('stops the command and fails when the run is aborted', async () => {
    const workspace = fakeWorkspace({});
    const abort = new AbortController();
    const job = runWorkspaceJob(
      workspace.client,
      { command: 'sleep 999', files: [], outputs: [], deadlineMs: Date.now() + 60_000 },
      { pollMs: 5, signal: abort.signal },
    );
    setTimeout(() => abort.abort(), 20);
    await expect(job).rejects.toThrow(/aborted/);
    expect(workspace.executed).toEqual(['start', 'kill']);
  });

  it('skips outputs beyond the size cap and drops files that changed after sizing', async () => {
    const workspace = fakeWorkspace({
      exitAfterPolls: 1,
      produce: { 'a.bin': Buffer.alloc(10), 'b.bin': Buffer.alloc(4) },
      sizes: { 'a.bin': 11, 'b.bin': MAX_OUTPUT_BYTES + 1 },
    });
    const outcome = await runWorkspaceJob(
      workspace.client,
      { command: 'true', files: [], outputs: ['a.bin', 'b.bin'], deadlineMs: Date.now() + 1_000 },
      { pollMs: 1 },
    );
    expect(outcome.outputs).toEqual([]);
    expect(outcome.missing).toEqual(['a.bin']);
    expect(outcome.skipped).toEqual(['b.bin']);
  });

  it('fails when a control snippet reports a kernel error', async () => {
    const workspace = fakeWorkspace({});
    workspace.client.execute.mockResolvedValueOnce({ ok: false, stdout: '', error: 'OSError: x' });
    await expect(
      runWorkspaceJob(workspace.client, {
        command: 'true',
        files: [],
        outputs: [],
        deadlineMs: Date.now() + 1_000,
      }),
    ).rejects.toThrow(/start failed: OSError/);
  });
});

describe('waiting for Jupyter to accept connections', () => {
  const endpoint = { origin: 'https://203.0.113.7:41234', token: 't'.repeat(32) };
  function client(responses: Array<number | string>) {
    const c = new WorkspaceClient(endpoint);
    const json = vi.spyOn(c, 'json').mockImplementation(async () => {
      const next = responses.shift() ?? 200;
      if (typeof next === 'string') throw new WorkspaceError(`request failed: ${next}`, next);
      return { status: next, body: null };
    });
    return { c, json };
  }

  it('retries while the port refuses connections, then proceeds', async () => {
    const { c, json } = client(['ECONNREFUSED', 'ECONNRESET', 502, 200]);
    await c.waitUntilServing(Date.now() + 5_000, { pollMs: 1 });
    expect(json).toHaveBeenCalledTimes(4);
    c.close();
  });

  it('fails at once on a certificate or token problem', async () => {
    const tls = client(['UNABLE_TO_VERIFY_LEAF_SIGNATURE']);
    await expect(tls.c.waitUntilServing(Date.now() + 5_000, { pollMs: 1 })).rejects.toThrow(
      /UNABLE_TO_VERIFY_LEAF_SIGNATURE/,
    );
    expect(tls.json).toHaveBeenCalledOnce();
    const token = client([403]);
    await expect(token.c.waitUntilServing(Date.now() + 5_000, { pollMs: 1 })).rejects.toThrow(
      /refused its token/,
    );
    tls.c.close();
    token.c.close();
  });

  it('gives up with the last failure at its deadline', async () => {
    const { c } = client(Array(100).fill('ECONNREFUSED'));
    await expect(c.waitUntilServing(Date.now() + 30, { pollMs: 5 })).rejects.toThrow(
      /ECONNREFUSED/,
    );
    c.close();
  });
});
