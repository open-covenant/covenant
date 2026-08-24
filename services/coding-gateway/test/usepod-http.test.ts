import { describe, expect, it, vi } from 'vitest';
import {
  accountUsePodTurn,
  boundedMaxTokens,
  probeUsePodBalance,
  probeUsePodCatalog,
  parseUsePodUsage,
  providerReceipt,
  usePodHeaders,
  usePodBalanceUrl,
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
    expect(usePodBalanceUrl(config)).toBe('https://api.usepod.test/proxy/funded%2Ftoken/balance');
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
      providerReportedCostMicrounits: '1700',
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
      providerReportedCostMicrounits: '2000001',
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

  it('accounts validated usage at the configured ceilings or a higher provider report', () => {
    const response = (reported?: string) =>
      new Response(null, {
        headers: {
          'x-pod-route': 'marketplace',
          'x-balance-remaining': '8000000',
          ...(reported ? { 'x-balance-cost-microunits': reported } : {}),
        },
      });
    const usage = { promptTokens: 10, completionTokens: 2 };

    expect(
      accountUsePodTurn(providerReceipt(response(), config.model), usage, 200_000, 400_000),
    ).toMatchObject({
      accounting: {
        accountedCostMicrounits: '3',
        basis: 'configured-price-ceilings',
        inputTokens: 10,
        outputTokens: 2,
        inputPriceMicrounitsPerMillion: 200_000,
        outputPriceMicrounitsPerMillion: 400_000,
      },
    });
    expect(
      accountUsePodTurn(providerReceipt(response(), config.model), usage, 200_000, 400_000)
        .providerReportedCostMicrounits,
    ).toBeUndefined();
    expect(
      accountUsePodTurn(providerReceipt(response('9'), config.model), usage, 200_000, 400_000),
    ).toMatchObject({
      providerReportedCostMicrounits: '9',
      accounting: {
        accountedCostMicrounits: '9',
        basis: 'max-of-configured-price-ceilings-and-provider-report',
      },
    });
    expect(
      accountUsePodTurn(providerReceipt(response('1'), config.model), usage, 200_000, 400_000),
    ).toMatchObject({
      providerReportedCostMicrounits: '1',
      accounting: {
        accountedCostMicrounits: '3',
        basis: 'max-of-configured-price-ceilings-and-provider-report',
      },
    });
  });

  it('checks the exact model with a bounded non-billable catalog request', async () => {
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(init?.method).toBe('GET');
      expect(new Headers(init?.headers)).toEqual(new Headers({ accept: 'application/json' }));
      return Response.json({
        object: 'list',
        data: [{ id: 'other-model' }, { id: config.model }],
      });
    });

    await expect(probeUsePodCatalog(config, request)).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://api.usepod.test/proxy/funded%2Ftoken/v1/models',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('accepts above-floor balance evidence from the non-billable endpoint', async () => {
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(init?.method).toBe('GET');
      expect(new Headers(init?.headers)).toEqual(
        new Headers({ accept: 'application/json', 'cache-control': 'no-cache' }),
      );
      expect(init?.cache).toBe('no-store');
      expect(init?.redirect).toBe('error');
      return Response.json(
        { usdc_balance: 2_000_000 },
        { headers: { 'x-balance-remaining': '2000000' } },
      );
    });

    await expect(probeUsePodBalance(config, request)).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://api.usepod.test/proxy/funded%2Ftoken/balance',
      expect.objectContaining({ method: 'GET' }),
    );

    await expect(
      probeUsePodBalance(
        config,
        vi.fn<typeof fetch>(async () => Response.json({ usdc_balance: 2_000_000 })),
      ),
    ).resolves.toBeUndefined();
  });

  it('rejects below-floor, conflicting, duplicate, and malformed balance evidence', async () => {
    const response = (body: string, header: string, contentType = 'application/json') =>
      vi.fn<typeof fetch>(
        async () =>
          new Response(body, {
            headers: {
              'content-type': contentType,
              'x-balance-remaining': header,
            },
          }),
      );

    await expect(
      probeUsePodBalance(config, response('{"usdc_balance":1999999}', '1999999')),
    ).rejects.toThrow(/below the configured/);
    await expect(
      probeUsePodBalance(config, response('{"usdc_balance":2000000}', '2000001')),
    ).rejects.toThrow(/conflicts/);
    await expect(
      probeUsePodBalance(config, response('{"usdc_balance":2000000}', '2000000, 2000000')),
    ).rejects.toThrow(/duplicate/);
    await expect(
      probeUsePodBalance(
        config,
        response('{"usdc_balance":1,"\\u0075sdc_balance":2000000}', '2000000'),
      ),
    ).rejects.toThrow(/duplicate JSON fields/);

    for (const body of [
      '{}',
      '{"usdc_balance":"2000000"}',
      '{"usdc_balance":1.5}',
      '{"usdc_balance":-1}',
      '{"usdc_balance":9007199254740992}',
    ]) {
      await expect(probeUsePodBalance(config, response(body, '2000000'))).rejects.toThrow(
        /invalid USDC microunits/,
      );
    }
    await expect(
      probeUsePodBalance(config, response('{"usdc_balance":2000000}', '2000000', 'text/plain')),
    ).rejects.toThrow(/non-JSON/);

    await expect(
      probeUsePodBalance(
        config,
        vi.fn<typeof fetch>(async () =>
          Response.json(
            { usdc_balance: 2_000_000 },
            {
              headers: {
                'content-length': '16385',
                'x-balance-remaining': '2000000',
              },
            },
          ),
        ),
      ),
    ).rejects.toThrow(/size limit/);
  });

  it('rejects oversized or malformed model catalogs', async () => {
    const oversized = vi.fn<typeof fetch>(async () =>
      Response.json(
        { object: 'list', data: [{ id: config.model }] },
        { headers: { 'content-length': '1048577' } },
      ),
    );
    await expect(probeUsePodCatalog(config, oversized)).rejects.toThrow(/size limit/);

    const malformed = vi.fn<typeof fetch>(async () =>
      Response.json({ object: 'list', data: [{ id: 7 }] }),
    );
    await expect(probeUsePodCatalog(config, malformed)).rejects.toThrow(/malformed JSON/);
  });
});
