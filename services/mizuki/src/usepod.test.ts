import { describe, expect, it, vi } from 'vitest';
import {
  boundedMaxTokens,
  matchesUsePodModel,
  publicUsePodReceipt,
  parseUsePodReviewDecision,
  parseUsePodUsage,
  probeUsePodCatalog,
  readUsePodChatCompletion,
  usePodHeaders,
  usePodReceipt,
  usePodUrl,
  type UsePodRequestConfig,
} from './usepod.js';

const config: UsePodRequestConfig = {
  baseUrl: 'https://api.usepod.test',
  token: 'funded/token',
  model: 'reviewer-model',
  maxInputPriceMicrounits: 200_000,
  maxOutputPriceMicrounits: 400_000,
  minimumBalance: '2000000',
};

describe('UsePod request contract', () => {
  it('uses token-in-path auth and both price ceilings', () => {
    expect(usePodUrl(config, 'chat/completions')).toBe(
      'https://api.usepod.test/proxy/funded%2Ftoken/v1/chat/completions',
    );
    expect(usePodHeaders(config)).toMatchObject({
      'x-pod-routing-mode': 'marketplace-only',
      'x-pod-max-price-input': '200000',
      'x-pod-max-price-output': '400000',
    });
    expect(usePodHeaders(config)).not.toHaveProperty('authorization');
  });

  it('requires funded marketplace response headers', () => {
    expect(() =>
      usePodReceipt(
        new Response(null, {
          headers: { 'x-pod-route': 'centralized', 'x-balance-remaining': '1000' },
        }),
        config.model,
        config.minimumBalance,
      ),
    ).toThrow(/unacceptable route/);
    expect(() =>
      usePodReceipt(
        new Response(null, {
          headers: { 'x-pod-route': 'marketplace', 'x-balance-remaining': '0' },
        }),
        config.model,
        config.minimumBalance,
      ),
    ).toThrow(/funded-balance/);
  });

  it('retains auditable route data without exposing the remaining balance', () => {
    const receipt = usePodReceipt(
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': '9000000',
          'x-pod-provider-id': 'provider-2',
          'x-request-id': 'request-9',
          'x-balance-cost-microunits': '500',
        },
      }),
      config.model,
      config.minimumBalance,
    );
    expect(publicUsePodReceipt(receipt)).toEqual({
      model: config.model,
      route: 'marketplace',
      providerId: 'provider-2',
      requestId: 'request-9',
      costMicrounits: '500',
    });

    const resolved = usePodReceipt(
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': '9000000',
        },
      }),
      'deepseek-v4-flash',
      '1',
      'deepseek-v4-flash-260425',
    );
    expect(publicUsePodReceipt(resolved)).toMatchObject({
      model: 'deepseek-v4-flash',
      resolvedModel: 'deepseek-v4-flash-260425',
    });
  });

  it('rejects insufficient balances, malformed costs, and invalid token usage', () => {
    const response = (balance: string, cost: string) =>
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': balance,
          'x-balance-cost-microunits': cost,
        },
      });
    expect(() => usePodReceipt(response('1999999', '1'), config.model, '2000000')).toThrow(
      /funded-balance floor/,
    );
    expect(usePodReceipt(response('2000000', '2000001'), config.model, '1')).toMatchObject({
      costMicrounits: '2000001',
    });
    expect(() => usePodReceipt(response('2000000', '1.5'), config.model, '1')).toThrow(
      /invalid cost receipt/,
    );
    expect(() =>
      usePodReceipt(
        response('2000000', (BigInt(Number.MAX_SAFE_INTEGER) + 1n).toString()),
        config.model,
        '1',
      ),
    ).toThrow(/invalid cost receipt/);
    expect(() => parseUsePodUsage({ prompt_tokens: -1, completion_tokens: 1 })).toThrow(
      /invalid token usage/,
    );
    expect(() => parseUsePodUsage({ prompt_tokens: 1.25, completion_tokens: 1 })).toThrow(
      /invalid token usage/,
    );
  });

  it('derives a conservative output token limit from a per-request budget', () => {
    const payload = { model: config.model, messages: [{ role: 'user', content: 'review' }] };
    const tokens = boundedMaxTokens(payload, 5_000, 200_000, 400_000, 1_000);
    const inputCost = Math.ceil((Buffer.byteLength(JSON.stringify(payload)) * 200_000) / 1_000_000);
    const outputCost = Math.ceil((tokens * 400_000) / 1_000_000);
    expect(tokens).toBeLessThanOrEqual(1_000);
    expect(inputCost + outputCost).toBeLessThanOrEqual(5_000);
  });

  it('binds the reviewer alias to the exact marketplace canonical identity', () => {
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek-v4-flash')).toBe(true);
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek-v4-flash-0731')).toBe(true);
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek-v4-flash-260425')).toBe(true);
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek/deepseek-v4-flash-0731')).toBe(true);
    for (const returned of [
      'deepseek/deepseek-v4-flash',
      'deepseek/deepseek-v4-flash-0730',
      'deepseek/deepseek-v4-flash-07310',
      'deepseek-v4-flash-260426',
      'deepseek-v4-flash-vision-exp',
      'deepseek-v3.2',
      'DeepSeek/deepseek-v4-flash-0731',
      'deepseek/deepseek-v4-flash-0731 ',
      undefined,
    ]) {
      expect(matchesUsePodModel('deepseek-v4-flash', returned)).toBe(false);
    }
    expect(matchesUsePodModel('deepseek/deepseek-v4-flash-0731', 'deepseek-v4-flash')).toBe(false);
    expect(matchesUsePodModel('constructor', 'deepseek/deepseek-v4-flash-0731')).toBe(false);
  });

  it('bounds and validates receipt identifiers', () => {
    const response = (headers: Record<string, string>) =>
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': '9000000',
          ...headers,
        },
      });

    expect(() =>
      usePodReceipt(response({ 'x-request-id': 'x'.repeat(129) }), config.model),
    ).toThrow(/invalid x-request-id receipt/);
    expect(() =>
      usePodReceipt(response({ 'x-pod-provider-id': 'provider id' }), config.model),
    ).toThrow(/invalid x-pod-provider-id receipt/);
    expect(() => usePodReceipt(response({}), 'model with spaces')).toThrow(
      /invalid model identity/,
    );
  });

  it.each([401, 403])('fails catalog readiness on HTTP %i', async (status) => {
    const request = async () => new Response(null, { status });

    await expect(probeUsePodCatalog(config, request as typeof fetch)).rejects.toThrow(
      `HTTP ${status}`,
    );
  });

  it('rejects malformed and oversized model catalogs', async () => {
    await expect(
      probeUsePodCatalog(config, (async () =>
        Response.json({ object: 'list', data: 'invalid' })) as typeof fetch),
    ).rejects.toThrow(/malformed JSON/);

    await expect(
      probeUsePodCatalog(
        config,
        (async () =>
          new Response(' '.repeat(1_048_577), {
            headers: { 'content-type': 'application/json' },
          })) as typeof fetch,
      ),
    ).rejects.toThrow(/response size limit/);
  });

  it('uses a bounded timeout without exposing the tokenized catalog URL', async () => {
    const timeout = AbortSignal.abort(new DOMException('timed out', 'TimeoutError'));
    const timeoutSpy = vi.spyOn(AbortSignal, 'timeout').mockReturnValue(timeout);
    const request = vi.fn<typeof fetch>(async (input, init) => {
      expect(init?.signal).toBe(timeout);
      throw new Error(`request failed at ${String(input)}`);
    });

    try {
      await expect(probeUsePodCatalog(config, request)).rejects.toThrow(
        'UsePod model catalog request failed',
      );
      let failure: unknown;
      try {
        await probeUsePodCatalog(config, request);
      } catch (cause) {
        failure = cause;
      }
      expect(String(failure)).not.toContain('funded%2Ftoken');
      expect(timeoutSpy).toHaveBeenCalledWith(15_000);
    } finally {
      timeoutSpy.mockRestore();
    }
  });
});

describe('UsePod chat completion responses', () => {
  it('accepts the bounded direct JSON shape and retains usage and canonical model evidence', async () => {
    const result = await readUsePodChatCompletion(
      Response.json({
        id: 'chatcmpl-1',
        object: 'chat.completion',
        model: 'deepseek-v4-flash-260425',
        choices: [
          {
            index: 0,
            finish_reason: 'stop',
            message: {
              role: 'assistant',
              reasoning_content: '',
              content: '{"approved":true,"reason":"scoped"}',
            },
          },
        ],
        usage: { prompt_tokens: 41, completion_tokens: 9, total_tokens: 50 },
      }),
    );

    expect(result).toEqual({
      ok: true,
      model: 'deepseek-v4-flash-260425',
      content: '{"approved":true,"reason":"scoped"}',
      usage: { prompt_tokens: 41, completion_tokens: 9, total_tokens: 50 },
    });
  });

  it.each(['text/event-stream', 'text/plain'])(
    'accepts strict JSON independently of a %s MIME label',
    async (contentType) => {
      const body = JSON.stringify({
        id: 'chatcmpl-1',
        model: 'deepseek-v4-flash-260425',
        choices: [
          {
            index: 0,
            finish_reason: 'stop',
            message: { role: 'assistant', content: '{"approved":true,"reason":"scoped"}' },
          },
        ],
        usage: { prompt_tokens: 41, completion_tokens: 9 },
      });

      await expect(
        readUsePodChatCompletion(new Response(body, { headers: { 'content-type': contentType } })),
      ).resolves.toMatchObject({ ok: true, model: 'deepseek-v4-flash-260425' });
    },
  );

  it('aggregates a mislabeled SSE response with a final usage-only frame', async () => {
    const result = await readUsePodChatCompletion(
      sseResponse(
        [
          chunk({ role: 'assistant', content: '', reasoning_content: null }),
          chunk({ content: '{"approved":' }),
          chunk({ content: 'true,"reason":"scoped"}' }),
          chunk({}, 'stop'),
          JSON.stringify({
            model: 'deepseek-v4-flash-0731',
            choices: [],
            usage: { prompt_tokens: 45, completion_tokens: 11, total_tokens: 56 },
          }),
          '[DONE]',
        ],
        'application/json',
      ),
    );

    expect(result).toEqual({
      ok: true,
      model: 'deepseek-v4-flash-0731',
      content: '{"approved":true,"reason":"scoped"}',
      usage: { prompt_tokens: 45, completion_tokens: 11, total_tokens: 56 },
    });
  });

  it('accepts CRLF-framed SSE and a usage frame without a request ID', async () => {
    const result = await readUsePodChatCompletion(
      sseResponse(
        [chunk({ content: '{"approved":true}' }, 'stop'), usageChunk(), '[DONE]'],
        'text/event-stream',
        '\r\n',
      ),
    );

    expect(result).toMatchObject({
      ok: true,
      model: 'deepseek-v4-flash-0731',
      content: '{"approved":true}',
    });
  });

  it('accepts mislabeled SSE after a BOM, whitespace, and standard comments', async () => {
    const framed = [
      ': first keepalive',
      ': second keepalive',
      '',
      `data: ${chunk({ content: '{"approved":true,"reason":"scoped"}' }, 'stop')}`,
      '',
      `data: ${usageChunk()}`,
      '',
      'data: [DONE]',
      '',
    ].join('\n');

    await expect(
      readUsePodChatCompletion(
        new Response(`\uFEFF \n${framed}`, {
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).resolves.toMatchObject({ ok: true, model: 'deepseek-v4-flash-0731' });
  });

  it('decodes Unicode split across transport chunks', async () => {
    const encoded = new TextEncoder().encode(
      JSON.stringify({
        model: 'deepseek-v4-flash-260425',
        choices: [
          {
            index: 0,
            finish_reason: 'stop',
            message: { role: 'assistant', content: '{"approved":true,"reason":"安全"}' },
          },
        ],
        usage: { prompt_tokens: 20, completion_tokens: 8 },
      }),
    );
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const byte of encoded) controller.enqueue(Uint8Array.of(byte));
        controller.close();
      },
    });

    await expect(
      readUsePodChatCompletion(
        new Response(stream, { headers: { 'content-type': 'application/json' } }),
      ),
    ).resolves.toMatchObject({
      ok: true,
      content: '{"approved":true,"reason":"安全"}',
    });
  });

  it('rejects missing stops, multiple choices, and tool-bearing messages without losing evidence', async () => {
    const usage = { prompt_tokens: 20, completion_tokens: 8 };
    for (const choices of [
      [
        {
          finish_reason: 'stop',
          message: { role: 'assistant', content: '{"approved":true,"reason":"scoped"}' },
        },
      ],
      [
        {
          index: 0,
          message: { role: 'assistant', content: '{"approved":true,"reason":"scoped"}' },
        },
      ],
      [
        {
          index: 0,
          finish_reason: 'stop',
          message: { role: 'assistant', content: '{}', tool_calls: [] },
        },
      ],
      [
        {
          index: 0,
          finish_reason: 'stop',
          message: { role: 'assistant', content: '{}', refusal: 'not available' },
        },
      ],
      [
        {
          index: 0,
          finish_reason: 'length',
          message: { role: 'assistant', content: '{}' },
        },
      ],
      [
        { index: 0, finish_reason: 'stop', message: { content: '{}' } },
        { index: 1, finish_reason: 'stop', message: { content: '{}' } },
      ],
    ]) {
      await expect(
        readUsePodChatCompletion(
          Response.json({ model: 'deepseek-v4-flash-260425', choices, usage }),
        ),
      ).resolves.toEqual({
        ok: false,
        error: 'UsePod review returned a malformed completion',
        retryable: true,
        model: 'deepseek-v4-flash-260425',
        usage,
      });
    }
  });

  it('rejects mixed success and error envelopes', async () => {
    const usage = { prompt_tokens: 20, completion_tokens: 8 };
    const completion = {
      model: 'deepseek-v4-flash-260425',
      error: { message: 'private-provider-diagnostic' },
      choices: [
        {
          index: 0,
          finish_reason: 'stop',
          message: { role: 'assistant', content: '{"approved":true,"reason":"scoped"}' },
        },
      ],
      usage,
    };
    await expect(readUsePodChatCompletion(Response.json(completion))).resolves.toMatchObject({
      ok: false,
      error: 'UsePod review returned a malformed completion',
      model: completion.model,
      usage,
    });

    const mixedChunk = JSON.stringify({
      id: 'request-1',
      model: 'deepseek-v4-flash-0731',
      error: { message: 'private-provider-diagnostic' },
      choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
      usage: null,
    });
    await expect(
      readUsePodChatCompletion(sseResponse([mixedChunk, usageChunk(), '[DONE]'])),
    ).resolves.toMatchObject({
      ok: false,
      error: 'UsePod review stream contained a malformed chunk',
    });
  });

  it.each([
    {
      name: 'changed model',
      frames: [chunk({ content: '{' }), chunk({ content: '}' }, 'stop', 'other-model')],
      error: 'changed model identity',
      retryable: false,
    },
    {
      name: 'changed request',
      frames: [chunk({ content: '{' }), chunk({ content: '}' }, 'stop', undefined, 'request-2')],
      error: 'changed request identity',
      retryable: false,
    },
    {
      name: 'missing terminator',
      frames: [chunk({ content: '{}' }, 'stop'), usageChunk()],
      error: 'ended before completion',
      retryable: true,
    },
    {
      name: 'content after stop',
      frames: [chunk({}, 'stop'), chunk({ content: '{}' }), usageChunk(), '[DONE]'],
      error: 'continued after completion',
      retryable: true,
    },
    {
      name: 'conflicting usage',
      frames: [
        chunk({}, 'stop'),
        usageChunk(),
        JSON.stringify({
          model: 'deepseek-v4-flash-0731',
          choices: [],
          usage: { prompt_tokens: 46, completion_tokens: 11, total_tokens: 57 },
        }),
        '[DONE]',
      ],
      error: 'conflicting usage',
      retryable: false,
    },
    {
      name: 'missing usage',
      frames: [chunk({}, 'stop'), '[DONE]'],
      error: 'ended before completion',
      retryable: true,
    },
    {
      name: 'multiple choices',
      frames: [
        JSON.stringify({
          id: 'request-1',
          model: 'deepseek-v4-flash-0731',
          choices: [
            { index: 0, delta: { content: '{}' }, finish_reason: null },
            { index: 1, delta: { content: '{}' }, finish_reason: null },
          ],
          usage: null,
        }),
        '[DONE]',
      ],
      error: 'malformed chunk',
      retryable: true,
    },
    {
      name: 'tool delta',
      frames: [
        JSON.stringify({
          id: 'request-1',
          model: 'deepseek-v4-flash-0731',
          choices: [{ index: 0, delta: { tool_calls: [] }, finish_reason: 'stop' }],
          usage: null,
        }),
        usageChunk(),
        '[DONE]',
      ],
      error: 'malformed chunk',
      retryable: true,
    },
    {
      name: 'invalid request identity',
      frames: [
        JSON.stringify({
          id: 'request identity with spaces',
          model: 'deepseek-v4-flash-0731',
          choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
          usage: null,
        }),
        usageChunk(),
        '[DONE]',
      ],
      error: 'invalid request identity',
      retryable: false,
    },
    {
      name: 'duplicate terminator',
      frames: [chunk({}, 'stop'), usageChunk(), '[DONE]', '[DONE]'],
      error: 'duplicate terminators',
      retryable: true,
    },
    {
      name: 'trailing completion data',
      frames: [chunk({}, 'stop'), usageChunk(), '[DONE]', usageChunk()],
      error: 'continued after its terminator',
      retryable: true,
    },
  ])('rejects an inconsistent SSE stream: $name', async ({ frames, error, retryable }) => {
    await expect(readUsePodChatCompletion(sseResponse(frames))).resolves.toMatchObject({
      ok: false,
      error: expect.stringContaining(error),
      retryable,
    });
  });

  it('rejects oversized bodies before parsing either framing mode', async () => {
    await expect(
      readUsePodChatCompletion(
        new Response(' '.repeat(64 * 1024 + 1), {
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).resolves.toEqual({
      ok: false,
      error: 'UsePod review response exceeded the size limit',
      retryable: true,
    });
  });

  it('rejects missing and invalid usage while retaining the reported evidence', async () => {
    const base = {
      model: 'deepseek-v4-flash-260425',
      choices: [{ index: 0, finish_reason: 'stop', message: { content: '{}' } }],
    };
    await expect(readUsePodChatCompletion(Response.json(base))).resolves.toMatchObject({
      ok: false,
      error: 'UsePod review omitted token usage',
      retryable: true,
      model: base.model,
    });

    const usage = { prompt_tokens: -1, completion_tokens: 8 };
    await expect(readUsePodChatCompletion(Response.json({ ...base, usage }))).resolves.toEqual({
      ok: false,
      error: 'UsePod review returned invalid token usage',
      retryable: false,
      model: base.model,
      usage,
    });

    await expect(
      readUsePodChatCompletion(
        sseResponse([
          chunk({}, 'stop'),
          JSON.stringify({ model: 'deepseek-v4-flash-0731', choices: [], usage }),
          '[DONE]',
        ]),
      ),
    ).resolves.toMatchObject({
      ok: false,
      error: 'UsePod review stream returned invalid token usage',
      retryable: false,
      usage,
    });
  });

  it('accepts repeated identical final usage without double-counting it', async () => {
    await expect(
      readUsePodChatCompletion(
        sseResponse([chunk({ content: '{}' }, 'stop'), usageChunk(), usageChunk(), '[DONE]']),
      ),
    ).resolves.toMatchObject({
      ok: true,
      usage: { prompt_tokens: 45, completion_tokens: 11, total_tokens: 56 },
    });
  });

  it('rejects invalid UTF-8 and unsupported response types without echoing the body', async () => {
    const invalid = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(Uint8Array.of(0xc3, 0x28));
        controller.close();
      },
    });
    await expect(
      readUsePodChatCompletion(
        new Response(invalid, { headers: { 'content-type': 'application/json' } }),
      ),
    ).resolves.toEqual({
      ok: false,
      error: 'UsePod review response could not be read',
      retryable: true,
    });

    const secret = 'private-provider-diagnostic';
    const result = await readUsePodChatCompletion(
      new Response(secret, { headers: { 'content-type': 'text/plain' } }),
    );
    expect(result).toEqual({
      ok: false,
      error: 'UsePod review returned an unsupported response format',
      retryable: true,
    });
    expect(JSON.stringify(result)).not.toContain(secret);
  });
});

describe('UsePod review decisions', () => {
  it('accepts exactly one bounded boolean decision and reason', () => {
    expect(parseUsePodReviewDecision('{"approved":true,"reason":"scoped \\u2713"}')).toEqual({
      approved: true,
      reason: 'scoped ✓',
    });
  });

  it.each([
    '{"approved":true,"approved":false,"reason":"private-provider-diagnostic"}',
    '{"approved":true,"\\u0061pproved":false,"reason":"private-provider-diagnostic"}',
    '{"approved":true,"reason":"ok","extra":true}',
    '{"approved":"yes","reason":"ok"}',
    '{"approved":true,"reason":""}',
    '{"approved":true,"reason":"private-provider-diagnostic"',
  ])('rejects an invalid or duplicate decision without exposing its content %#', (content) => {
    let failure: unknown;
    try {
      parseUsePodReviewDecision(content);
    } catch (cause) {
      failure = cause;
    }
    expect(String(failure)).toBe('Error: UsePod reviewer returned an invalid decision');
    expect(String(failure)).not.toContain('private-provider-diagnostic');
  });

  it('rejects a reason above the fixed decision bound', () => {
    expect(() =>
      parseUsePodReviewDecision(JSON.stringify({ approved: true, reason: 'x'.repeat(2_001) })),
    ).toThrow('UsePod reviewer returned an invalid decision');
  });
});

function chunk(
  delta: { role?: 'assistant'; content?: string; reasoning_content?: string | null },
  finishReason: 'stop' | null = null,
  model = 'deepseek-v4-flash-0731',
  id = 'request-1',
): string {
  return JSON.stringify({
    id,
    object: 'chat.completion.chunk',
    model,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
    usage: null,
  });
}

function usageChunk(): string {
  return JSON.stringify({
    model: 'deepseek-v4-flash-0731',
    choices: [],
    usage: { prompt_tokens: 45, completion_tokens: 11, total_tokens: 56 },
  });
}

function sseResponse(
  frames: string[],
  contentType = 'text/event-stream',
  lineEnding = '\n',
): Response {
  return new Response(frames.map((frame) => `data: ${frame}${lineEnding}${lineEnding}`).join(''), {
    headers: { 'content-type': contentType },
  });
}
