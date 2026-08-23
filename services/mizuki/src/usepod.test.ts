import { describe, expect, it } from 'vitest';
import {
  boundedMaxTokens,
  publicUsePodReceipt,
  parseUsePodUsage,
  probeUsePod,
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

  it('fails reviewer readiness below the configured balance floor', async () => {
    const request = async () =>
      Response.json(
        {
          model: config.model,
          choices: [{ message: { content: JSON.stringify({ nonce: 'mizuki-ready' }) } }],
        },
        {
          headers: {
            'x-pod-route': 'marketplace',
            'x-balance-remaining': '1999999',
          },
        },
      );

    await expect(probeUsePod(config, request as typeof fetch)).rejects.toThrow(
      /funded-balance floor/,
    );
  });
});
