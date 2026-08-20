import { describe, expect, it, vi } from 'vitest';

import { callRobinhoodTool, robinhoodTools } from '../server';

function mockFetch(status: number, body: unknown) {
  return vi.fn(async (_input: string | URL | Request, _init?: RequestInit) =>
    new Response(typeof body === 'string' ? body : JSON.stringify(body), { status }),
  );
}

describe('robinhood-mcp-bridge', () => {
  it('exposes the governed tool set', () => {
    const names = robinhoodTools.map((t) => t.name);
    expect(names).toContain('robinhood_quote');
    expect(names).toContain('robinhood_governed_order');
    expect(names).toContain('robinhood_reputation');
  });

  it('proxies a quote to the daemon tools/call with the right shape and auth', async () => {
    const fetchImpl = mockFetch(200, { results: [{ symbol: 'BTC-USD', price: '60000' }] });
    const env = { COVENANT_HTTP_URL: 'http://127.0.0.1:8421', COVENANT_AUTH_TOKEN: 'tok' } as NodeJS.ProcessEnv;
    const res = await callRobinhoodTool('robinhood_quote', { symbols: ['BTC-USD'] }, { env, fetchImpl });

    expect(res?.isError).toBeFalsy();
    expect(fetchImpl).toHaveBeenCalledOnce();
    const [url, init] = fetchImpl.mock.calls[0]!;
    expect(String(url)).toBe('http://127.0.0.1:8421/tools/call');
    expect(init!.method).toBe('POST');
    expect(JSON.parse(init!.body as string)).toEqual({ name: 'robinhood.quote', arguments: { symbols: ['BTC-USD'] } });
    expect((init!.headers as Record<string, string>).Authorization).toBe('Bearer tok');
  });

  it('routes a governed order to place_order and never signs locally', async () => {
    const fetchImpl = mockFetch(200, { decision: 'executed', venue_order_id: 'ord_1' });
    const res = await callRobinhoodTool(
      'robinhood_governed_order',
      { symbol: 'BTC-USD', side: 'buy', quantity: 0.001 },
      { env: {} as NodeJS.ProcessEnv, fetchImpl },
    );
    expect(res?.isError).toBeFalsy();
    const [, init] = fetchImpl.mock.calls[0]!;
    expect(JSON.parse(init!.body as string).name).toBe('robinhood.place_order');
  });

  it('rejects an invalid order before any network call', async () => {
    const fetchImpl = mockFetch(200, {});
    const res = await callRobinhoodTool(
      'robinhood_governed_order',
      { symbol: 'BTC-USD', side: 'buy', quantity: -1 },
      { env: {} as NodeJS.ProcessEnv, fetchImpl },
    );
    expect(res?.isError).toBe(true);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it('redacts the auth token from daemon errors', async () => {
    const fetchImpl = mockFetch(401, 'denied for token supersecret');
    const env = { COVENANT_AUTH_TOKEN: 'supersecret' } as NodeJS.ProcessEnv;
    const res = await callRobinhoodTool('robinhood_account', {}, { env, fetchImpl });
    expect(res?.isError).toBe(true);
    expect(res?.content[0]?.text).not.toContain('supersecret');
  });
});
