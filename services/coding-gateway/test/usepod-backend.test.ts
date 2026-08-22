import { afterEach, describe, expect, it, vi } from 'vitest';
import { UsePodBackend } from '../src/backends/usepod.js';
import type { GatewayEvent, Sandbox } from '../src/types.js';

afterEach(() => vi.unstubAllGlobals());

describe('UsePodBackend', () => {
  it('uses marketplace routing and executes tool calls inside the sandbox', async () => {
    const requests: RequestInit[] = [];
    let turn = 0;
    vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
      requests.push(init);
      turn++;
      return Response.json(
        turn === 1
          ? {
              choices: [
                {
                  message: {
                    content: null,
                    tool_calls: [
                      {
                        id: 'call-1',
                        function: {
                          name: 'write_file',
                          arguments: JSON.stringify({ path: 'README.md', content: 'fixed' }),
                        },
                      },
                    ],
                  },
                },
              ],
              usage: { prompt_tokens: 20, completion_tokens: 4 },
            }
          : {
              choices: [{ message: { content: 'Done.' } }],
              usage: { prompt_tokens: 30, completion_tokens: 5 },
            },
      );
    });
    const files: Record<string, string> = {};
    const sandbox = {
      readFile: async (path: string) => files[path] ?? '',
      writeFile: async (path: string, content: string) => {
        files[path] = content;
      },
      exec: async () => ({ stdout: '', stderr: '', exitCode: 0 }),
      previewUrl: async () => '',
      destroy: async () => {},
    } satisfies Sandbox;
    const events: GatewayEvent[] = [];

    const result = await new UsePodBackend(
      'https://usepod.test/v1',
      'test-key',
      'deepseek-v3.2',
    ).run({
      input: 'fix docs',
      sandbox,
      signal: new AbortController().signal,
      emit: (event) => events.push(event),
    });

    expect(files['README.md']).toBe('fixed');
    expect(result.usage).toMatchObject({ inputTokens: 50, outputTokens: 9 });
    expect(events.map((event) => event.type)).toContain('file.written');
    expect(new Headers(requests[0]!.headers).get('x-pod-routing-mode')).toBe('marketplace-only');
    expect(new Headers(requests[0]!.headers).get('x-pod-no-retention')).toBe('true');
  });

  it('refuses to run without a key', async () => {
    const backend = new UsePodBackend('https://usepod.test/v1', '', 'deepseek-v3.2');
    await expect(
      backend.run({
        input: 'x',
        sandbox: {} as Sandbox,
        signal: new AbortController().signal,
        emit: () => {},
      }),
    ).rejects.toThrow('USEPOD_API_KEY is required');
  });
});
