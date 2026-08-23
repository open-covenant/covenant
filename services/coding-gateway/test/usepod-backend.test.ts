import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { UsePodBackend } from '../src/backends/usepod.js';
import { RunStore, type StoredRun } from '../src/run-store.js';
import type { GatewayEvent, ProviderReceipt, Sandbox } from '../src/types.js';

const directories: string[] = [];

afterEach(() => {
  vi.unstubAllGlobals();
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true, force: true });
});

describe('UsePodBackend', () => {
  it('uses marketplace routing and executes tool calls inside the sandbox', async () => {
    const requests: RequestInit[] = [];
    const urls: string[] = [];
    let turn = 0;
    vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
      urls.push(url);
      requests.push(init);
      turn++;
      return Response.json(
        turn === 1
          ? {
              model: 'deepseek-v3.2',
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
              model: 'deepseek-v3.2',
              choices: [{ message: { content: 'Done.' } }],
              usage: { prompt_tokens: 30, completion_tokens: 5 },
            },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
            'x-pod-provider-id': 'provider-1',
            'x-balance-cost-microunits': '1250',
          },
        },
      );
    });
    const files: Record<string, string> = {};
    const order: string[] = [];
    const sandbox = {
      readFile: async (path: string) => files[path] ?? '',
      writeFile: async (path: string, content: string) => {
        order.push('tool');
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
      maxProviderCostUsd: 1,
      recordProviderReceipt: () => {
        order.push('receipt');
      },
    });

    expect(files['README.md']).toBe('fixed');
    expect(result.usage).toMatchObject({ inputTokens: 50, outputTokens: 9 });
    expect(result.providerReceipts).toHaveLength(2);
    expect(result.providerReceipts[0]).toMatchObject({
      model: 'deepseek-v3.2',
      route: 'marketplace',
      providerId: 'provider-1',
      providerReportedCostMicrounits: '1250',
      accounting: {
        accountedCostMicrounits: '1250',
        basis: 'max-of-configured-price-ceilings-and-provider-report',
        inputTokens: 20,
        outputTokens: 4,
        inputPriceMicrounitsPerMillion: 200000,
        outputPriceMicrounitsPerMillion: 400000,
      },
    });
    expect(events.map((event) => event.type)).toContain('file.written');
    expect(order).toEqual(['receipt', 'tool', 'receipt']);
    expect(new Headers(requests[0]!.headers).get('x-pod-routing-mode')).toBe('marketplace-only');
    expect(new Headers(requests[0]!.headers).get('x-pod-no-retention')).toBe('true');
    expect(new Headers(requests[0]!.headers).get('authorization')).toBeNull();
    expect(new Headers(requests[0]!.headers).get('x-pod-max-price-input')).toBe('200000');
    expect(new Headers(requests[0]!.headers).get('x-pod-max-price-output')).toBe('400000');
    expect(urls[0]).toBe('https://usepod.test/proxy/test-key/v1/chat/completions');
  });

  it('checkpoints a billed turn before a later response failure and preserves it on restart', async () => {
    vi.stubGlobal('fetch', async () =>
      Response.json(
        { model: 'deepseek-v3.2', choices: [], usage: { prompt_tokens: 3, completion_tokens: 1 } },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
            'x-balance-cost-microunits': '1250',
          },
        },
      ),
    );
    const directory = mkdtempSync(join(tmpdir(), 'mizuki-paid-turn-'));
    directories.push(directory);
    const path = join(directory, 'runs.json');
    const store = new RunStore(path);
    const run: StoredRun = {
      id: 'run-paid-turn',
      sessionId: 'job-1:implementation',
      requestFingerprint: 'a'.repeat(64),
      reservationId: 'reservation-1',
      reservedMax: 2,
      status: 'running',
      events: [],
      updatedAt: new Date().toISOString(),
    };
    store.save(run);

    await expect(
      new UsePodBackend('https://usepod.test', 'test-key', 'deepseek-v3.2').run({
        input: 'fix docs',
        sandbox: {} as Sandbox,
        signal: new AbortController().signal,
        emit: () => {},
        maxProviderCostUsd: 1,
        recordProviderReceipt: (receipt) => {
          run.providerReceipts = [...(run.providerReceipts ?? []), receipt];
          store.save(run);
        },
      }),
    ).rejects.toThrow(/no completion choice/);

    expect(new RunStore(path).list()[0]).toMatchObject({
      status: 'failed',
      costUsd: 2,
      providerReceipts: [
        {
          route: 'marketplace',
          providerReportedCostMicrounits: '1250',
          accounting: { accountedCostMicrounits: '1250' },
        },
      ],
    });
  });

  it('falls back to the reservation when response usage cannot be accounted', async () => {
    vi.stubGlobal('fetch', async () =>
      Response.json(
        {
          model: 'deepseek-v3.2',
          choices: [{ message: { content: 'Done.' } }],
          usage: { prompt_tokens: -1, completion_tokens: 1 },
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
            'x-balance-cost-microunits': '1250',
          },
        },
      ),
    );
    const receipts: ProviderReceipt[] = [];

    await expect(
      new UsePodBackend('https://usepod.test', 'test-key', 'deepseek-v3.2').run({
        input: 'fix docs',
        sandbox: {} as Sandbox,
        signal: new AbortController().signal,
        emit: () => {},
        maxProviderCostUsd: 1,
        recordProviderReceipt: (receipt) => receipts.push(receipt),
      }),
    ).rejects.toThrow(/invalid token usage/);
    expect(receipts).toHaveLength(0);
  });

  it('completes thirty turns without allowing receipt spend past the run cap', async () => {
    let turn = 0;
    const maxTokens: number[] = [];
    vi.stubGlobal('fetch', async (_url: string, init: RequestInit) => {
      turn += 1;
      maxTokens.push((JSON.parse(String(init.body)) as { max_tokens: number }).max_tokens);
      const message =
        turn === 30
          ? { content: 'Done.' }
          : {
              content: null,
              tool_calls: [
                {
                  id: `call-${turn}`,
                  function: { name: 'read_file', arguments: JSON.stringify({ path: 'README.md' }) },
                },
              ],
            };
      return Response.json(
        {
          model: 'deepseek-v3.2',
          choices: [{ message }],
          usage: { prompt_tokens: 10, completion_tokens: 2 },
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
            'x-balance-cost-microunits': '10000',
          },
        },
      );
    });
    const receipts: ProviderReceipt[] = [];
    const sandbox = {
      readFile: async () => '',
      writeFile: async () => {},
      exec: async () => ({ stdout: '', stderr: '', exitCode: 0 }),
      previewUrl: async () => '',
      destroy: async () => {},
    } satisfies Sandbox;

    const result = await new UsePodBackend('https://usepod.test', 'test-key', 'deepseek-v3.2').run({
      input: 'fix docs',
      sandbox,
      signal: new AbortController().signal,
      emit: () => {},
      maxProviderCostUsd: 0.3,
      recordProviderReceipt: (receipt) => receipts.push(receipt),
    });

    const spent = receipts.reduce(
      (sum, receipt) => sum + BigInt(receipt.accounting!.accountedCostMicrounits),
      0n,
    );
    expect(result.output).toBe('Done.');
    expect(turn).toBe(30);
    expect(spent).toBe(300_000n);
    expect(maxTokens.every((value) => value > 0 && value <= 16_000)).toBe(true);
  });

  it('accounts multi-turn usage when provider cost reports are omitted', async () => {
    let turn = 0;
    const request = vi.fn(async () => {
      turn += 1;
      return Response.json(
        {
          model: 'deepseek-v3.2',
          choices:
            turn === 1
              ? [
                  {
                    message: {
                      content: null,
                      tool_calls: [
                        {
                          id: 'call-1',
                          function: {
                            name: 'read_file',
                            arguments: JSON.stringify({ path: 'README.md' }),
                          },
                        },
                      ],
                    },
                  },
                ]
              : [{ message: { content: 'Done.' } }],
          usage: { prompt_tokens: 10, completion_tokens: 2 },
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
          },
        },
      );
    });
    vi.stubGlobal('fetch', request);
    const receipts: ProviderReceipt[] = [];

    const result = await new UsePodBackend('https://usepod.test', 'test-key', 'deepseek-v3.2').run({
      input: 'fix docs',
      sandbox: { readFile: async () => '' } as Sandbox,
      signal: new AbortController().signal,
      emit: () => {},
      maxProviderCostUsd: 1,
      recordProviderReceipt: (receipt) => receipts.push(receipt),
    });

    expect(result.output).toBe('Done.');
    expect(request).toHaveBeenCalledTimes(2);
    expect(receipts).toHaveLength(2);
    expect(receipts).toEqual([
      expect.objectContaining({
        accounting: expect.objectContaining({
          accountedCostMicrounits: '3',
          basis: 'configured-price-ceilings',
        }),
      }),
      expect.objectContaining({
        accounting: expect.objectContaining({
          accountedCostMicrounits: '3',
          basis: 'configured-price-ceilings',
        }),
      }),
    ]);
    expect(receipts[0]?.providerReportedCostMicrounits).toBeUndefined();
  });

  it('keeps an over-budget receipt visible before aborting', async () => {
    vi.stubGlobal('fetch', async () =>
      Response.json(
        {
          model: 'deepseek-v3.2',
          choices: [{ message: { content: 'Done.' } }],
          usage: { prompt_tokens: 1, completion_tokens: 1 },
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '5000000',
            'x-balance-cost-microunits': '1250',
          },
        },
      ),
    );
    const receipts: ProviderReceipt[] = [];
    await expect(
      new UsePodBackend('https://usepod.test', 'test-key', 'deepseek-v3.2').run({
        input: 'fix docs',
        sandbox: {} as Sandbox,
        signal: new AbortController().signal,
        emit: () => {},
        maxProviderCostUsd: 0.001,
        recordProviderReceipt: (receipt) => receipts.push(receipt),
      }),
    ).rejects.toThrow(/exceeded the run budget/);
    expect(receipts).toMatchObject([
      {
        providerReportedCostMicrounits: '1250',
        accounting: { accountedCostMicrounits: '1250' },
      },
    ]);
  });

  it('refuses to run without a key', async () => {
    const backend = new UsePodBackend('https://usepod.test/v1', '', 'deepseek-v3.2');
    await expect(
      backend.run({
        input: 'x',
        sandbox: {} as Sandbox,
        signal: new AbortController().signal,
        emit: () => {},
        maxProviderCostUsd: 1,
      }),
    ).rejects.toThrow('USEPOD_API_KEY is required');
  });
});
