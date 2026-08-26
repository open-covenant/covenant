import { describe, expect, it, vi } from 'vitest';
import { MizukiMcpClient } from './mcp-api.js';

describe('Mizuki MCP API client', () => {
  it('adds a scoped bearer token and timeout signal to maintainer reads', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example/',
      apiToken: 'mzk_v1_machine-token',
      timeoutMs: 2_000,
      request,
    });

    await expect(client.repositories()).resolves.toEqual({ repositories: [] });

    const [url, init] = vi.mocked(request).mock.calls[0]!;
    const headers = new Headers(init?.headers);
    expect(url).toBe('https://mizuki.example/v1/account/repositories');
    expect(init?.method).toBe('GET');
    expect(init?.signal).toBeInstanceOf(AbortSignal);
    expect(headers.get('authorization')).toBe('Bearer mzk_v1_machine-token');
    expect(headers.has('cookie')).toBe(false);
  });

  it('fails closed before making an unauthenticated maintainer request', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({ baseUrl: 'https://mizuki.example', request });

    await expect(client.repositories()).rejects.toThrow('MIZUKI_API_TOKEN');
    await expect(client.jobs()).rejects.toThrow('MIZUKI_API_TOKEN');
    expect(request).not.toHaveBeenCalled();
  });

  it('lists account jobs with the scoped bearer token and no browser credentials', async () => {
    const request = response({ jobs: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example',
      apiToken: 'mzk_v1_machine-token',
      request,
    });

    await expect(client.jobs()).resolves.toEqual({ jobs: [] });

    const [url, init] = vi.mocked(request).mock.calls[0]!;
    const headers = new Headers(init?.headers);
    expect(url).toBe('https://mizuki.example/v1/account/jobs?limit=20');
    expect(init?.method).toBe('GET');
    expect(headers.get('authorization')).toBe('Bearer mzk_v1_machine-token');
    expect(headers.has('cookie')).toBe(false);
  });

  it('bounds account job history requests from one through one hundred', async () => {
    const request = response({ jobs: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example',
      apiToken: 'mzk_v1_machine-token',
      request,
    });

    await client.jobs(1);
    await client.jobs(100);
    await expect(client.jobs(0)).rejects.toThrow('between 1 and 100');
    await expect(client.jobs(101)).rejects.toThrow('between 1 and 100');
    await expect(client.jobs(1.5)).rejects.toThrow('between 1 and 100');

    expect(vi.mocked(request).mock.calls.map(([url]) => url)).toEqual([
      'https://mizuki.example/v1/account/jobs?limit=1',
      'https://mizuki.example/v1/account/jobs?limit=100',
    ]);
  });

  it('uses authenticated readiness, issue, preflight, and payment recovery routes', async () => {
    const request = response({ repositories: [] });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example',
      apiToken: 'mzk_v1_machine-token',
      request,
    });

    await expect(client.repositoryReadiness('open-covenant', 'covenant')).resolves.toMatchObject({
      repository: 'open-covenant/covenant',
      status: 'not_connected',
    });
    await client.quote('https://github.com/open-covenant/covenant/issues/1');
    await client.issues('open-covenant', 'covenant');
    await client.preflight('https://github.com/open-covenant/covenant/issues/1');
    await client.paymentStatus('11111111-1111-4111-8111-111111111111', 'stable-payment-key');

    const calls = vi.mocked(request).mock.calls;
    expect(calls.map(([url]) => url)).toEqual([
      'https://mizuki.example/v1/account/repositories',
      'https://mizuki.example/v1/account/quotes',
      'https://mizuki.example/v1/repositories/open-covenant/covenant/issues',
      'https://mizuki.example/v1/preflights',
      'https://mizuki.example/v1/account/quotes/11111111-1111-4111-8111-111111111111/payment-status',
    ]);
    expect(calls.every(([, init]) => init?.signal instanceof AbortSignal)).toBe(true);
    expect(calls[1]![1]?.body).toBe(
      JSON.stringify({
        github_issue_url: 'https://github.com/open-covenant/covenant/issues/1',
      }),
    );
    expect(calls[3]![1]?.body).toBe(
      JSON.stringify({
        github_issue_url: 'https://github.com/open-covenant/covenant/issues/1',
      }),
    );
    expect(new Headers(calls[4]![1]?.headers).get('idempotency-key')).toBe('stable-payment-key');
    expect(calls.every(([, init]) => new Headers(init?.headers).has('payment-signature'))).toBe(
      false,
    );
    expect(
      calls.every(
        ([, init]) =>
          new Headers(init?.headers).get('authorization') === 'Bearer mzk_v1_machine-token' &&
          !new Headers(init?.headers).has('cookie'),
      ),
    ).toBe(true);
  });

  it('keeps quotes public when the MCP host has no API token', async () => {
    const request = response({ id: 'quote' });
    const client = new MizukiMcpClient({ baseUrl: 'https://mizuki.example', request });

    await client.quote('https://github.com/open-covenant/covenant/issues/1');

    expect(vi.mocked(request).mock.calls[0]?.[0]).toBe('https://mizuki.example/v1/quotes');
    const headers = new Headers(vi.mocked(request).mock.calls[0]?.[1]?.headers);
    expect(headers.has('cookie')).toBe(false);
    expect(headers.has('authorization')).toBe(false);
  });

  it('never forwards caller-supplied browser credentials', async () => {
    const request = response({ ok: true });
    const client = new MizukiMcpClient({ baseUrl: 'https://mizuki.example', request });

    await client.call('/v1/health', {
      headers: {
        authorization: 'Bearer caller-supplied',
        cookie: 'mizuki_session=browser-session',
      },
    });

    const headers = new Headers(vi.mocked(request).mock.calls[0]?.[1]?.headers);
    expect(headers.has('authorization')).toBe(false);
    expect(headers.has('cookie')).toBe(false);
  });

  it('rejects unbounded timeout configuration', () => {
    expect(
      () => new MizukiMcpClient({ baseUrl: 'https://mizuki.example', timeoutMs: 999 }),
    ).toThrow('between 1000 and 60000');
    expect(
      () => new MizukiMcpClient({ baseUrl: 'https://mizuki.example', timeoutMs: 60_001 }),
    ).toThrow('between 1000 and 60000');
  });

  it('requires encrypted transport outside loopback development', () => {
    const credentialUrl = new URL('https://api.example/v1');
    credentialUrl.username = 'caller';
    credentialUrl.password = 'credential';

    expect(() => new MizukiMcpClient({ baseUrl: 'not a URL' })).toThrow('invalid');
    expect(() => new MizukiMcpClient({ baseUrl: 'http://api.example' })).toThrow('HTTPS');
    expect(() => new MizukiMcpClient({ baseUrl: 'ftp://api.example' })).toThrow('HTTPS');
    expect(() => new MizukiMcpClient({ baseUrl: credentialUrl.toString() })).toThrow('credentials');
    expect(() => new MizukiMcpClient({ baseUrl: 'http://127.0.0.1:8787' })).not.toThrow();
  });

  it('does not turn a response-body timeout into a successful tool result', async () => {
    const request = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const signal = init?.signal;
      return {
        ok: true,
        status: 200,
        json: () =>
          new Promise((_resolve, reject) => {
            signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
          }),
      } as Response;
    });
    const client = new MizukiMcpClient({
      baseUrl: 'https://mizuki.example',
      timeoutMs: 1_000,
      request,
    });

    await expect(client.call('/v1/health')).rejects.toThrow('timed out after 1000ms');
  });

  it('rejects invalid JSON instead of returning it as a successful result', async () => {
    const request = vi.fn(async () => new Response('not-json', { status: 200 }));
    const client = new MizukiMcpClient({ baseUrl: 'https://mizuki.example', request });

    await expect(client.call('/v1/health')).rejects.toThrow('invalid JSON response');
  });
});

function response(value: unknown): typeof fetch {
  return vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => Response.json(value));
}
