import { afterEach, describe, expect, it, vi } from 'vitest';
import { GET, POST } from './route';

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe('Mizuki API proxy', () => {
  it('replaces untrusted context with the validated Cloudflare address on Render', async () => {
    vi.stubEnv('MIZUKI_API_URL', 'https://mizuki-api.onrender.com');
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'p'.repeat(32));
    vi.stubEnv('NEXT_PUBLIC_MIZUKI_APP_URL', 'https://mizuki.opencovenant.org');
    const upstream = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({ ok: true }, { headers: { 'set-cookie': 'session=upstream' } }),
    );
    vi.stubGlobal('fetch', upstream);

    const response = await POST(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/quotes?source=web', {
        method: 'POST',
        headers: {
          authorization: 'Bearer public-token',
          'cf-connecting-ip': '198.51.100.8',
          'content-type': 'application/json',
          'x-forwarded-for': '203.0.113.9, 198.51.100.8',
          'x-mizuki-client-ip': '203.0.113.10',
          'x-mizuki-forwarded-proto': 'http',
          'x-mizuki-proxy-secret': 'attacker-controlled',
        },
        body: '{}',
      }),
      { params: Promise.resolve({ path: ['v1', 'quotes'] }) },
    );

    expect(response.status).toBe(200);
    expect(response.headers.get('set-cookie')).toBe('session=upstream');
    expect(upstream).toHaveBeenCalledTimes(1);
    const [target, init] = upstream.mock.calls[0];
    const headers = new Headers(init?.headers);
    expect(target).toBe('https://mizuki-api.onrender.com/v1/quotes?source=web');
    expect(headers.get('authorization')).toBe('Bearer public-token');
    expect(headers.get('cf-connecting-ip')).toBeNull();
    expect(headers.get('x-forwarded-for')).toBeNull();
    expect(headers.get('x-mizuki-client-ip')).toBe('198.51.100.8');
    expect(headers.get('x-mizuki-forwarded-proto')).toBe('https');
    expect(headers.get('x-mizuki-proxy-secret')).toBe('p'.repeat(32));
  });

  it('fails closed without proxy authentication and ignores spoofed XFF', async () => {
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', '');
    const upstream = vi.fn();
    vi.stubGlobal('fetch', upstream);
    const unavailable = await GET(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/activity'),
      { params: Promise.resolve({ path: ['v1', 'activity'] }) },
    );
    expect(unavailable.status).toBe(503);
    expect(upstream).not.toHaveBeenCalled();

    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'p'.repeat(32));
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ events: [] })),
    );
    await GET(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/activity', {
        headers: {
          'cf-connecting-ip': 'not-an-ip',
          'x-forwarded-for': '198.51.100.1',
        },
      }),
      { params: Promise.resolve({ path: ['v1', 'activity'] }) },
    );
    const replacement = vi.mocked(fetch).mock.calls[0][1];
    expect(new Headers(replacement?.headers).get('x-mizuki-client-ip')).toBeNull();

    await GET(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/activity', {
        headers: { 'x-forwarded-for': '198.51.100.2' },
      }),
      { params: Promise.resolve({ path: ['v1', 'activity'] }) },
    );
    const missing = vi.mocked(fetch).mock.calls[1][1];
    expect(new Headers(missing?.headers).get('x-mizuki-client-ip')).toBeNull();
  });

  it('requires a proxy secret of at least 32 UTF-8 bytes', async () => {
    const upstream = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({ ok: true }),
    );
    vi.stubGlobal('fetch', upstream);
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'é'.repeat(15));

    const rejected = await GET(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/activity'),
      {
        params: Promise.resolve({ path: ['v1', 'activity'] }),
      },
    );
    expect(rejected.status).toBe(503);
    expect(upstream).not.toHaveBeenCalled();

    const secret = 'é'.repeat(16);
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', secret);
    const accepted = await GET(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/activity'),
      {
        params: Promise.resolve({ path: ['v1', 'activity'] }),
      },
    );
    expect(accepted.status).toBe(200);
    expect(new Headers(upstream.mock.calls[0][1]?.headers).get('x-mizuki-proxy-secret')).toBe(
      secret,
    );
  });

  it('rejects an oversized Content-Length before forwarding', async () => {
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'p'.repeat(32));
    const upstream = vi.fn();
    vi.stubGlobal('fetch', upstream);

    const response = await POST(
      new Request('https://mizuki.opencovenant.org/api/mizuki/v1/quotes', {
        method: 'POST',
        headers: { 'content-length': '64001', 'content-type': 'application/json' },
        body: '{}',
      }),
      { params: Promise.resolve({ path: ['v1', 'quotes'] }) },
    );

    expect(response.status).toBe(413);
    await expect(response.json()).resolves.toEqual({ error: 'Request body exceeds 64000 bytes' });
    expect(upstream).not.toHaveBeenCalled();
  });

  it('caps unknown-length streams at the API body limit', async () => {
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'p'.repeat(32));
    const upstream = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({ ok: true }),
    );
    vi.stubGlobal('fetch', upstream);

    const accepted = await POST(streamedRequest([64_000]), {
      params: Promise.resolve({ path: ['v1', 'quotes'] }),
    });
    expect(accepted.status).toBe(200);
    const forwardedBody = upstream.mock.calls[0][1]?.body;
    expect(forwardedBody).toBeInstanceOf(ArrayBuffer);
    expect((forwardedBody as ArrayBuffer).byteLength).toBe(64_000);

    upstream.mockClear();
    const rejected = await POST(streamedRequest([64_000, 1]), {
      params: Promise.resolve({ path: ['v1', 'quotes'] }),
    });
    expect(rejected.status).toBe(413);
    expect(upstream).not.toHaveBeenCalled();
  });

  it('forwards signed GitHub webhooks with a dedicated one-megabyte cap', async () => {
    vi.stubEnv('MIZUKI_WEB_PROXY_SECRET', 'p'.repeat(32));
    const upstream = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      Response.json({ accepted: true }, { status: 202 }),
    );
    vi.stubGlobal('fetch', upstream);
    const path = ['v1', 'github', 'webhook'];
    const headers = {
      'content-type': 'application/json',
      'x-github-delivery': 'delivery-1',
      'x-github-event': 'pull_request',
      'x-hub-signature-256': `sha256=${'a'.repeat(64)}`,
    };

    const accepted = await POST(
      streamedRequest(
        [1_000_000],
        'https://mizuki.opencovenant.org/api/mizuki/v1/github/webhook',
        headers,
      ),
      { params: Promise.resolve({ path }) },
    );
    expect(accepted.status).toBe(202);
    const forwarded = new Headers(upstream.mock.calls[0][1]?.headers);
    expect(forwarded.get('x-github-delivery')).toBe('delivery-1');
    expect(forwarded.get('x-github-event')).toBe('pull_request');
    expect(forwarded.get('x-hub-signature-256')).toBe(`sha256=${'a'.repeat(64)}`);
    expect((upstream.mock.calls[0][1]?.body as ArrayBuffer).byteLength).toBe(1_000_000);

    upstream.mockClear();
    const rejected = await POST(
      streamedRequest(
        [1_000_000, 1],
        'https://mizuki.opencovenant.org/api/mizuki/v1/github/webhook',
        headers,
      ),
      { params: Promise.resolve({ path }) },
    );
    expect(rejected.status).toBe(413);
    await expect(rejected.json()).resolves.toEqual({
      error: 'Request body exceeds 1000000 bytes',
    });
    expect(upstream).not.toHaveBeenCalled();
  });
});

function streamedRequest(
  chunkSizes: number[],
  url = 'https://mizuki.opencovenant.org/api/mizuki/v1/quotes',
  headers: HeadersInit = { 'content-type': 'application/octet-stream' },
): Request {
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const size of chunkSizes) controller.enqueue(new Uint8Array(size));
      controller.close();
    },
  });
  return new Request(url, {
    method: 'POST',
    headers,
    body,
    duplex: 'half',
  } as RequestInit & { duplex: 'half' });
}
