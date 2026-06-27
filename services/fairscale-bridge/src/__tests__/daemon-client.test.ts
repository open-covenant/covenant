import { afterEach, describe, expect, it, vi } from 'vitest';
import { HttpDaemonClient } from '../daemon.js';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

function client(): HttpDaemonClient {
  return new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, 0);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('HttpDaemonClient response classification', () => {
  it('recentAudit throws on a daemon error response', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'error', message: 'upstream down' })));

    await expect(client().recentAudit({ limit: 10 })).rejects.toThrow('daemon audit error: upstream down');
  });

  it('recentAudit throws on an unexpected response kind', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'something_else' })));

    await expect(client().recentAudit({ limit: 10 })).rejects.toThrow(
      'unexpected daemon audit response: something_else',
    );
  });

  it('recentAudit returns the events for a valid audit_events response', async () => {
    const events = [
      { id: 'e1', timestamp_ms: 1, issuer: { display: 'a', pubkey: 'pk' }, kind: { type: 't' } },
    ];
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'audit_events', events })));

    await expect(client().recentAudit({ limit: 10 })).resolves.toEqual(events);
  });

  it('recentAudit sends limit always and since_ms only when sinceMs is given', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => jsonResponse({ kind: 'audit_events', events: [] }));
    vi.stubGlobal('fetch', fetchMock);
    const c = client();

    await c.recentAudit({ limit: 7, sinceMs: 1234 });
    await c.recentAudit({ limit: 7 });

    const urls = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(urls[0]).toContain('limit=7');
    expect(urls[0]).toContain('since_ms=1234');
    expect(urls[1]).toContain('limit=7');
    expect(urls[1]).not.toContain('since_ms');
  });

  it('verify throws when the daemon returns no report', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse({ kind: 'verify_report' })));

    await expect(client().verify()).rejects.toThrow('daemon verify returned no report');
  });
});

describe('HttpDaemonClient retry policy', () => {
  it('retries a 5xx daemon error', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(null, { status: 503 }));
    vi.stubGlobal('fetch', fetchMock);
    const c = new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, 1);

    await expect(c.recentAudit({ limit: 5 })).rejects.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('does not retry a 4xx daemon error', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(null, { status: 404 }));
    vi.stubGlobal('fetch', fetchMock);
    const c = new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, 1);

    await expect(c.recentAudit({ limit: 5 })).rejects.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('retries a transient network error', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => {
      throw new TypeError('network');
    });
    vi.stubGlobal('fetch', fetchMock);
    const c = new HttpDaemonClient('http://daemon.local', 'operator-token', 1000, 1);

    await expect(c.recentAudit({ limit: 5 })).rejects.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});

describe('HttpDaemonClient health probe', () => {
  it('reports healthy when the daemon /health responds ok', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 200 })));

    await expect(client().health()).resolves.toBe(true);
  });

  it('reports unhealthy on a non-2xx status instead of trusting the connection', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 503 })));

    await expect(client().health()).resolves.toBe(false);
  });

  it('fails closed to false on a network error rather than throwing', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      throw new TypeError('network');
    }));

    await expect(client().health()).resolves.toBe(false);
  });
});
