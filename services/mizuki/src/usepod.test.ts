import { describe, expect, it, vi } from 'vitest';
import {
  boundedMaxTokens,
  matchesUsePodModel,
  publicUsePodReceipt,
  parseUsePodUsage,
  probeUsePodCatalog,
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
    ).toThrow(/funded balance/);
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
    expect(matchesUsePodModel('deepseek-v4-flash', 'deepseek/deepseek-v4-flash-0731')).toBe(true);
    for (const returned of [
      'deepseek/deepseek-v4-flash',
      'deepseek/deepseek-v4-flash-0730',
      'deepseek/deepseek-v4-flash-07310',
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

  it('uses a bounded timeout for the catalog request', async () => {
    const timeout = AbortSignal.abort(new DOMException('timed out', 'TimeoutError'));
    const timeoutSpy = vi.spyOn(AbortSignal, 'timeout').mockReturnValue(timeout);
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(init?.signal).toBe(timeout);
      throw timeout.reason;
    });

    try {
      await expect(probeUsePodCatalog(config, request)).rejects.toMatchObject({
        name: 'TimeoutError',
      });
      expect(timeoutSpy).toHaveBeenCalledWith(15_000);
    } finally {
      timeoutSpy.mockRestore();
    }
  });
});
