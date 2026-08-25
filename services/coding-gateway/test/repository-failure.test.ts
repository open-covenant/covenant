import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';

const nativeFetch = globalThis.fetch;
const directories: string[] = [];

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.doUnmock('../src/backends/index.js');
  vi.doUnmock('../src/sandbox/local.js');
  vi.resetModules();
  for (const directory of directories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe('repository run failure capture', () => {
  it('does not fall back to a generic workspace snapshot after the file limit fails', async () => {
    const directory = mkdtempSync(join(tmpdir(), 'mizuki-repository-failure-'));
    directories.push(directory);
    const token = 'g'.repeat(32);
    vi.stubEnv('NODE_ENV', 'test');
    vi.stubEnv('CODER_AUTH_TOKEN', token);
    vi.stubEnv('CODER_BACKEND', 'openai');
    vi.stubEnv('CODER_MODEL', 'test-model');
    vi.stubEnv('OPENAI_API_KEY', 'test-key');
    vi.stubEnv('ALLOW_LOCAL_REPOSITORY_RUNS', '1');
    vi.stubEnv('E2B_API_KEY', '');
    vi.stubEnv('RUN_STORE_PATH', join(directory, 'runs.json'));
    vi.stubEnv('LEDGER_PATH', join(directory, 'ledger.json'));

    const changes = Array.from({ length: 41 }, (_, index) => `cache-${index}.txt`).join('\0');
    const exec = vi.fn(async (command: string) => ({
      stdout: command.includes('git ls-files') ? `${changes}\0` : '',
      stderr: '',
      exitCode: 0,
    }));
    const readFile = vi.fn(async () => 'unexpected');
    const sandbox = {
      readFile,
      writeFile: vi.fn(async () => {}),
      exec,
      previewUrl: vi.fn(async () => ''),
      destroy: vi.fn(async () => {}),
    };

    vi.doMock('../src/sandbox/local.js', () => ({
      LocalSandboxProvider: class {
        readonly id = 'local';
        async create() {
          return sandbox;
        }
        async check() {}
      },
    }));
    vi.doMock('../src/backends/index.js', () => ({
      selectBackend: () => ({
        id: 'openai',
        async run() {
          return {
            output: 'done',
            usage: {
              inputTokens: 1,
              outputTokens: 1,
              cacheReadTokens: 0,
              cacheCreationTokens: 0,
            },
          };
        },
      }),
    }));
    vi.stubGlobal('fetch', async (input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(input instanceof Request ? input.url : String(input));
      if (url.hostname === 'api.openai.com') {
        return Response.json({ data: [{ id: 'test-model' }] });
      }
      return nativeFetch(input, init);
    });

    const { server } = await import('../src/server.js');
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('gateway did not bind');
    const origin = `http://127.0.0.1:${address.port}`;
    const headers = {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
    };

    try {
      const response = await nativeFetch(`${origin}/v1/runs`, {
        method: 'POST',
        headers,
        body: JSON.stringify({
          session_id: 'repository-limit-failure',
          input: 'fix docs',
          max_cost_usd: 1,
          repository_url: 'https://github.com/open-covenant/covenant',
        }),
      });
      expect(response.status).toBe(200);
      const { run_id: runId } = (await response.json()) as { run_id: string };

      let state: { status: string; error?: string } | undefined;
      for (let attempt = 0; attempt < 50; attempt++) {
        const current = await nativeFetch(`${origin}/v1/runs/${runId}`, { headers });
        state = (await current.json()) as { status: string; error?: string };
        if (state.status !== 'running') break;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }

      expect(state).toMatchObject({
        status: 'failed',
        error: 'repository change exceeds the 40-file capture limit',
      });
      const files = await nativeFetch(`${origin}/v1/runs/${runId}/files`, { headers });
      await expect(files.json()).resolves.toEqual({ files: [] });
      expect(exec.mock.calls.some(([command]) => String(command).startsWith('find .'))).toBe(false);
      expect(readFile).not.toHaveBeenCalled();
    } finally {
      await new Promise<void>((resolve, reject) => {
        server.close((cause) => (cause ? reject(cause) : resolve()));
      });
    }
  });
});
