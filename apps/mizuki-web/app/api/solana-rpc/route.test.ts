import { afterEach, describe, expect, it, vi } from 'vitest';
import { POST } from './route';

const endpoint = 'https://mainnet.helius-rpc.com/?api-key=test-key';
const mint = 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v';
const confirmed = { commitment: 'confirmed' };

afterEach(() => {
  vi.restoreAllMocks();
  delete process.env.MIZUKI_SOLANA_RPC_URL;
});

describe('Solana payment RPC', () => {
  it('forwards only the payment client read methods without exposing the credential', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const upstream = vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ jsonrpc: '2.0', id: 7, result: { value: 'blockhash' } }), {
        headers: { 'content-type': 'application/json' },
      }),
    );

    const response = await POST(
      request({ jsonrpc: '2.0', id: 7, method: 'getLatestBlockhash', params: [confirmed] }),
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      jsonrpc: '2.0',
      id: 7,
      result: { value: 'blockhash' },
    });
    expect(upstream).toHaveBeenCalledWith(
      endpoint,
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({
          jsonrpc: '2.0',
          id: 7,
          method: 'getLatestBlockhash',
          params: [confirmed],
        }),
      }),
    );
    expect(JSON.stringify(await response.headers)).not.toContain('test-key');
  });

  it('forwards only the canonical USDC mint account read', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const upstream = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(Response.json({ jsonrpc: '2.0', id: 8, result: { value: null } }));

    const response = await POST(
      request(
        {
          jsonrpc: '2.0',
          id: 8,
          method: 'getAccountInfo',
          params: [mint, { encoding: 'base64', commitment: 'confirmed' }],
        },
        '203.0.113.8',
      ),
    );

    expect(response.status).toBe(200);
    expect(upstream).toHaveBeenCalledOnce();
  });

  it.each(['getBalance', 'sendTransaction', 'simulateTransaction'])(
    'rejects unsupported method %s',
    async (method) => {
      process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
      const upstream = vi.spyOn(globalThis, 'fetch');

      const response = await POST(
        request({ jsonrpc: '2.0', id: 1, method, params: [] }, `203.0.113.${method.length}`),
      );

      expect(response.status).toBe(400);
      expect(upstream).not.toHaveBeenCalled();
    },
  );

  it('rejects batch requests', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const response = await POST(request([{ jsonrpc: '2.0', id: 1, method: 'getAccountInfo' }]));
    expect(response.status).toBe(400);
  });

  it('rejects arbitrary account reads and changed RPC options', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const upstream = vi.spyOn(globalThis, 'fetch');

    for (const [index, params] of [
      ['11111111111111111111111111111111', { encoding: 'base64', commitment: 'confirmed' }],
      [mint, { encoding: 'base64' }],
      [mint, { encoding: 'jsonParsed', commitment: 'confirmed' }],
      [mint, { encoding: 'base64', commitment: 'finalized' }],
      [mint, { encoding: 'base64', commitment: 'confirmed', dataSlice: { offset: 0, length: 1 } }],
    ].entries()) {
      const response = await POST(
        request(
          { jsonrpc: '2.0', id: index, method: 'getAccountInfo', params },
          `198.51.100.${index + 1}`,
        ),
      );
      expect(response.status).toBe(400);
    }
    expect(upstream).not.toHaveBeenCalled();
  });

  it('rejects changed blockhash options', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const upstream = vi.spyOn(globalThis, 'fetch');

    for (const [index, params] of [
      [],
      [{ commitment: 'finalized' }],
      [{ commitment: 'confirmed', minContextSlot: 1 }],
    ].entries()) {
      const response = await POST(
        request(
          { jsonrpc: '2.0', id: index, method: 'getLatestBlockhash', params },
          `192.0.2.${index + 60}`,
        ),
      );
      expect(response.status).toBe(400);
    }
    expect(upstream).not.toHaveBeenCalled();
  });

  it('rejects cross-origin browser requests', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const response = await POST(
      request(
        { jsonrpc: '2.0', id: 1, method: 'getLatestBlockhash', params: [confirmed] },
        '192.0.2.10',
        { origin: 'https://example.com' },
      ),
    );
    expect(response.status).toBe(403);
  });

  it('rate limits each source', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    vi.spyOn(globalThis, 'fetch').mockImplementation(async () =>
      Response.json({ jsonrpc: '2.0', id: 1, result: {} }),
    );
    for (let index = 0; index < 30; index += 1) {
      const response = await POST(
        request(
          { jsonrpc: '2.0', id: index, method: 'getLatestBlockhash', params: [confirmed] },
          '192.0.2.30',
        ),
      );
      expect(response.status).toBe(200);
    }
    const blocked = await POST(
      request(
        { jsonrpc: '2.0', id: 31, method: 'getLatestBlockhash', params: [confirmed] },
        '192.0.2.30',
      ),
    );
    expect(blocked.status).toBe(429);
  });

  it('aborts an oversized upstream response', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('x'.repeat(64_001)));

    const response = await POST(
      request(
        { jsonrpc: '2.0', id: 1, method: 'getLatestBlockhash', params: [confirmed] },
        '192.0.2.40',
      ),
    );

    expect(response.status).toBe(502);
  });

  it('fails closed when the server-side endpoint is missing', async () => {
    const response = await POST(
      request(
        { jsonrpc: '2.0', id: 1, method: 'getLatestBlockhash', params: [confirmed] },
        '192.0.2.50',
      ),
    );
    expect(response.status).toBe(503);
  });
});

function request(
  body: unknown,
  source = '203.0.113.1',
  extraHeaders: Record<string, string> = {},
): Request {
  return new Request('https://mizuki.opencovenant.org/api/solana-rpc', {
    method: 'POST',
    headers: {
      'cf-connecting-ip': source,
      'content-type': 'application/json',
      origin: 'https://mizuki.opencovenant.org',
      ...extraHeaders,
    },
    body: JSON.stringify(body),
  });
}
