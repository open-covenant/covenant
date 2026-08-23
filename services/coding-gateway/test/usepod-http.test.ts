import { describe, expect, it, vi } from 'vitest';
import {
  boundedMaxTokens,
  probeUsePod,
  parseUsePodUsage,
  providerReceipt,
  usePodHeaders,
  usePodUrl,
  type UsePodRequestConfig,
} from '../src/usepod-http.js';

const config: UsePodRequestConfig = {
  baseUrl: 'https://api.usepod.test/v1',
  token: 'funded/token',
  model: 'deepseek-v3.2',
  maxInputPriceMicrounits: 200_000,
  maxOutputPriceMicrounits: 400_000,
  minimumBalance: '2000000',
};

describe('UsePod HTTP contract', () => {
  it('uses token-in-path authentication and mandatory spend controls', () => {
    expect(usePodUrl(config, '/chat/completions')).toBe(
      'https://api.usepod.test/proxy/funded%2Ftoken/v1/chat/completions',
    );
    expect(usePodHeaders(config)).toEqual({
      'content-type': 'application/json',
      'x-pod-routing-mode': 'marketplace-only',
      'x-pod-no-retention': 'true',
      'x-pod-max-price-input': '200000',
      'x-pod-max-price-output': '400000',
    });
  });

  it('rejects unsafe base URLs and pre-tokenized paths', () => {
    expect(() => usePodUrl({ ...config, baseUrl: 'http://api.usepod.test' }, 'models')).toThrow(
      /HTTPS/,
    );
    expect(() =>
      usePodUrl({ ...config, baseUrl: 'https://api.usepod.test/proxy/exposed/v1' }, 'models'),
    ).toThrow(/tokenized proxy path/);
  });

  it('accepts only funded marketplace route receipts', () => {
    const response = new Response(null, {
      headers: {
        'x-pod-route': 'marketplace',
        'x-balance-remaining': '8000000',
        'x-pod-provider-id': 'provider-7',
        'x-request-id': 'request-3',
        'x-balance-cost-microunits': '1700',
      },
    });
    expect(providerReceipt(response, config.model, config.minimumBalance)).toEqual({
      model: config.model,
      route: 'marketplace',
      balanceRemaining: '8000000',
      providerId: 'provider-7',
      requestId: 'request-3',
      costMicrounits: '1700',
    });

    expect(() =>
      providerReceipt(
        new Response(null, {
          headers: { 'x-pod-route': 'centralized', 'x-balance-remaining': '1' },
        }),
        config.model,
      ),
    ).toThrow(/unacceptable route/);
    expect(() =>
      providerReceipt(
        new Response(null, {
          headers: { 'x-pod-route': 'marketplace', 'x-balance-remaining': '0' },
        }),
        config.model,
      ),
    ).toThrow(/funded balance/);
  });

  it('requires the configured balance floor and syntactically valid cost receipts', () => {
    const response = (balance: string, cost = '1000') =>
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': balance,
          'x-balance-cost-microunits': cost,
        },
      });

    expect(() => providerReceipt(response('1999999'), config.model, '2000000')).toThrow(
      /funded-balance floor/,
    );
    expect(providerReceipt(response('2000000', '2000001'), config.model, '1')).toMatchObject({
      costMicrounits: '2000001',
    });
    expect(() => providerReceipt(response('2000000', '1.5'), config.model, '1')).toThrow(
      /invalid cost receipt/,
    );
    expect(() =>
      providerReceipt(
        response('2000000', (BigInt(Math.floor(Number.MAX_SAFE_INTEGER / 30)) + 1n).toString()),
        config.model,
        '1',
      ),
    ).toThrow(/invalid cost receipt/);
  });

  it('bounds output tokens from the request budget and price ceilings', () => {
    const payload = { model: config.model, messages: [{ role: 'user', content: 'x' }] };
    const tokens = boundedMaxTokens(payload, 10_000, 200_000, 400_000, 16_000);
    const inputUpperBound = Buffer.byteLength(JSON.stringify(payload));
    const inputCost = Math.ceil((inputUpperBound * 200_000) / 1_000_000);
    const outputCost = Math.ceil((tokens * 400_000) / 1_000_000);
    expect(inputCost + outputCost).toBeLessThanOrEqual(10_000);
    expect(() => boundedMaxTokens(payload, 1, 200_000, 400_000, 16_000)).toThrow(/spend budget/);
  });

  it('accepts only safe integer token usage', () => {
    expect(parseUsePodUsage({ prompt_tokens: 10, completion_tokens: 3 })).toEqual({
      promptTokens: 10,
      completionTokens: 3,
    });
    for (const usage of [
      undefined,
      { prompt_tokens: -1, completion_tokens: 1 },
      { prompt_tokens: 1.5, completion_tokens: 1 },
      { prompt_tokens: Number.MAX_SAFE_INTEGER + 1, completion_tokens: 1 },
      { prompt_tokens: 1 },
    ]) {
      expect(() => parseUsePodUsage(usage)).toThrow(/invalid token usage/);
    }
  });

  it('proves the exact configured model can execute a funded tool call', async () => {
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(new Headers(init?.headers).get('authorization')).toBeNull();
      const body = JSON.parse(String(init?.body)) as {
        model: string;
        tool_choice: { function: { name: string } };
      };
      expect(body.model).toBe(config.model);
      expect(body.tool_choice.function.name).toBe('readiness_probe');
      return Response.json(
        {
          model: config.model,
          choices: [
            {
              message: {
                tool_calls: [
                  {
                    function: {
                      name: 'readiness_probe',
                      arguments: JSON.stringify({ nonce: 'mizuki-ready' }),
                    },
                  },
                ],
              },
            },
          ],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '7000000',
          },
        },
      );
    });

    await expect(probeUsePod(config, request)).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://api.usepod.test/proxy/funded%2Ftoken/v1/chat/completions',
      expect.objectContaining({ method: 'POST' }),
    );
  });

  it('fails readiness when the provider reports another model', async () => {
    const request = vi.fn<typeof fetch>(async () =>
      Response.json(
        {
          model: 'other-model',
          choices: [
            {
              message: {
                tool_calls: [
                  {
                    function: {
                      name: 'readiness_probe',
                      arguments: JSON.stringify({ nonce: 'mizuki-ready' }),
                    },
                  },
                ],
              },
            },
          ],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '7000000',
          },
        },
      ),
    );

    await expect(probeUsePod(config, request)).rejects.toThrow(/different model/);
  });

  it('fails readiness when the funded balance is below the configured floor', async () => {
    const request = vi.fn<typeof fetch>(async () =>
      Response.json(
        {
          model: config.model,
          choices: [
            {
              message: {
                tool_calls: [
                  {
                    function: {
                      name: 'readiness_probe',
                      arguments: JSON.stringify({ nonce: 'mizuki-ready' }),
                    },
                  },
                ],
              },
            },
          ],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '1999999',
          },
        },
      ),
    );

    await expect(probeUsePod(config, request)).rejects.toThrow(/funded-balance floor/);
  });
});
