import { afterEach, describe, expect, it, vi } from 'vitest';
import { POST } from './route';

const endpoint = 'https://mainnet.helius-rpc.com/?api-key=test-key';

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
      request({ jsonrpc: '2.0', id: 7, method: 'getLatestBlockhash', params: [] }),
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
          params: [],
        }),
      }),
    );
    expect(JSON.stringify(await response.headers)).not.toContain('test-key');
  });

  it.each(['getBalance', 'sendTransaction', 'simulateTransaction'])(
    'rejects unsupported method %s',
    async (method) => {
      process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
      const upstream = vi.spyOn(globalThis, 'fetch');

      const response = await POST(request({ jsonrpc: '2.0', id: 1, method, params: [] }));

      expect(response.status).toBe(400);
      expect(upstream).not.toHaveBeenCalled();
    },
  );

  it('rejects batch requests', async () => {
    process.env.MIZUKI_SOLANA_RPC_URL = endpoint;
    const response = await POST(request([{ jsonrpc: '2.0', id: 1, method: 'getAccountInfo' }]));
    expect(response.status).toBe(400);
  });

  it('fails closed when the server-side endpoint is missing', async () => {
    const response = await POST(
      request({ jsonrpc: '2.0', id: 1, method: 'getAccountInfo', params: [] }),
    );
    expect(response.status).toBe(503);
  });
});

function request(body: unknown): Request {
  return new Request('https://mizuki.opencovenant.org/api/solana-rpc', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}
