import { describe, expect, it } from 'vitest';
import { callHermesTool, hermesTools } from '../server.js';

function fetchJson(
  handler: (url: URL, init: RequestInit) => { status?: number; body?: unknown },
): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = input instanceof URL ? input : new URL(String(input));
    const result = handler(url, init ?? {});
    return new Response(JSON.stringify(result.body ?? { ok: true }), {
      status: result.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

describe('Hermes MCP bridge', () => {
  it('advertises generic Hermes tools', () => {
    const names = hermesTools.map((tool) => tool.name);
    expect(names).toEqual([
      'hermes_health',
      'hermes_capabilities',
      'hermes_run',
      'hermes_run_status',
      'hermes_run_events',
      'hermes_stop',
    ]);
  });

  it('calls health and capabilities endpoints', async () => {
    const seen: string[] = [];
    const fetchImpl = fetchJson((url, init) => {
      seen.push(`${init.method ?? 'GET'} ${url.pathname}`);
      expect(init.headers).toMatchObject({ Authorization: 'Bearer hermes-key' });
      return { body: { ok: true } };
    });
    const env = {
      HERMES_API_BASE_URL: 'http://127.0.0.1:8642/v1',
      HERMES_API_KEY: 'hermes-key',
    };

    await callHermesTool('hermes_health', {}, { env, fetchImpl });
    await callHermesTool('hermes_capabilities', {}, { env, fetchImpl });

    expect(seen).toEqual(['GET /v1/health', 'GET /v1/capabilities']);
  });

  it('starts Hermes agent runs', async () => {
    const fetchImpl = fetchJson((url, init) => {
      expect(url.pathname).toBe('/v1/runs');
      expect(init.method).toBe('POST');
      expect(JSON.parse(String(init.body))).toMatchObject({
        input: 'summarize',
        session_id: 'planner',
      });
      return { body: { run_id: 'run_1', status: 'queued' } };
    });

    const result = await callHermesTool(
      'hermes_run',
      { prompt: 'summarize', session_id: 'planner' },
      { env: {}, fetchImpl },
    );

    expect(result?.isError).toBeUndefined();
    expect(result?.content[0]?.text).toContain('run_1');
  });

  it('handles status, event, and stop flows', async () => {
    const seen: string[] = [];
    const fetchImpl = fetchJson((url, init) => {
      seen.push(`${init.method ?? 'GET'} ${url.pathname}${url.search}`);
      return { body: { ok: true } };
    });

    await callHermesTool('hermes_run_status', { run_id: 'run/1' }, { env: {}, fetchImpl });
    await callHermesTool('hermes_run_events', { run_id: 'run/1', cursor: 'c1', limit: 2 }, { env: {}, fetchImpl });
    await callHermesTool('hermes_stop', { run_id: 'run/1' }, { env: {}, fetchImpl });

    expect(seen).toEqual([
      'GET /v1/runs/run%2F1',
      'GET /v1/runs/run%2F1/events?cursor=c1&limit=2',
      'POST /v1/runs/run%2F1/stop',
    ]);
  });

  it('redacts auth and base URL from API errors', async () => {
    const fetchImpl = fetchJson(() => ({
      status: 401,
      body: {
        error: 'bad token hermes-secret at http://127.0.0.1:9999/v1',
      },
    }));

    const result = await callHermesTool('hermes_health', {}, {
      env: {
        HERMES_API_BASE_URL: 'http://127.0.0.1:9999/v1',
        HERMES_API_KEY: 'hermes-secret',
      },
      fetchImpl,
    });

    expect(result?.isError).toBe(true);
    expect(result?.content[0]?.text).toContain('<redacted-token>');
    expect(result?.content[0]?.text).toContain('$HERMES_API_BASE_URL');
    expect(result?.content[0]?.text).not.toContain('hermes-secret');
    expect(result?.content[0]?.text).not.toContain('127.0.0.1:9999');
  });
});
