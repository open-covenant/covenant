import { describe, expect, it, vi } from 'vitest';
import { MizukiMcpClient } from './mcp-api.js';

describe('Mizuki MCP API client', () => {
  it('adds an authenticated session and timeout signal to maintainer reads', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example/',
      session: 'signed session',
      timeoutMs: 2_000,
      request,
    });

    await expect(client.repositories()).resolves.toEqual({ repositories: [] });

    const [url, init] = vi.mocked(request).mock.calls[0]!;
    const headers = new Headers(init?.headers);
    expect(url).toBe('https://mizuki.example/v1/account/repositories');
    expect(init?.method).toBe('GET');
    expect(init?.signal).toBeInstanceOf(AbortSignal);
    expect(headers.get('cookie')).toBe('mizuki_session=signed%20session');
  });

  it('fails closed before making an unauthenticated maintainer request', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({ baseUrl: 'https://mizuki.example', request });

    await expect(client.repositories()).rejects.toThrow('requires an authenticated');
    expect(request).not.toHaveBeenCalled();
  });

  it('uses authenticated readiness, issue, preflight, and payment recovery routes', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example',
      session: 'session',
      request,
    });

    await expect(client.repositoryReadiness('open-covenant', 'covenant')).resolves.toMatchObject({
      repository: 'open-covenant/covenant',
      status: 'not_connected',
    });
    await client.issues('open-covenant', 'covenant');
    await client.preflight('https://github.com/open-covenant/covenant/issues/1');
    await client.paymentStatus('11111111-1111-4111-8111-111111111111', 'stable-payment-key');

    const calls = vi.mocked(request).mock.calls;
    expect(calls.map(([url]) => url)).toEqual([
      'https://mizuki.example/v1/account/repositories',
      'https://mizuki.example/v1/repositories/open-covenant/covenant/issues',
      'https://mizuki.example/v1/preflights',
      'https://mizuki.example/v1/account/quotes/11111111-1111-4111-8111-111111111111/payment-status',
    ]);
    expect(calls.every(([, init]) => init?.signal instanceof AbortSignal)).toBe(true);
    expect(calls[2]![1]?.body).toBe(
      JSON.stringify({
        github_issue_url: 'https://github.com/open-covenant/covenant/issues/1',
      }),
    );
    expect(new Headers(calls[3]![1]?.headers).get('idempotency-key')).toBe('stable-payment-key');
    expect(calls.every(([, init]) => new Headers(init?.headers).has('payment-signature'))).toBe(
      false,
    );
  });

  it('rejects unbounded timeout configuration', () => {
    expect(
      () => new MizukiMcpClient({ baseUrl: 'https://mizuki.example', timeoutMs: 999 }),
    ).toThrow('between 1000 and 60000');
    expect(
      () => new MizukiMcpClient({ baseUrl: 'https://mizuki.example', timeoutMs: 60_001 }),
    ).toThrow('between 1000 and 60000');
  });
});

function response(value: unknown): typeof fetch {
  return vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => Response.json(value));
}
