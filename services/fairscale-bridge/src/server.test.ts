import { afterEach, describe, expect, it } from 'vitest';
import { buildServer } from './server.js';
import type { Config } from './config.js';
import type { AuditEvent, DaemonClient, IntegrityReport } from './daemon.js';

const cfg: Config = {
  port: 0,
  daemonUrl: 'http://daemon',
  daemonToken: 'op-token',
  daemonTimeoutMs: 1000,
  daemonRetries: 0,
  apiToken: 'fairscale-test-token-0123456789',
  maxLimit: 1000,
  defaultLimit: 100,
  fetchCap: 100_000,
};

const auth = { authorization: `Bearer ${cfg.apiToken}` };
const report: IntegrityReport = { events: 4, anchors: 1, valid: true, root_hash_hex: 'deadbeef', failures: [] };

function ev(id: string, ms: number, issuer: string, kind: AuditEvent['kind']): AuditEvent {
  return { id, timestamp_ms: ms, issuer: { display: issuer, pubkey: `pk_${issuer}` }, kind };
}

// Mirrors the daemon: events at/after sinceMs, newest `limit` (tail), ascending.
class FakeDaemon implements DaemonClient {
  constructor(private events: AuditEvent[], private opts: { verifyError?: boolean } = {}) {}
  async recentAudit({ sinceMs, limit }: { sinceMs?: number; limit: number }): Promise<AuditEvent[]> {
    let out = [...this.events].sort((a, b) => a.timestamp_ms - b.timestamp_ms || (a.id < b.id ? -1 : 1));
    if (sinceMs !== undefined) out = out.filter((e) => e.timestamp_ms >= sinceMs);
    return out.slice(Math.max(0, out.length - limit));
  }
  async verify(): Promise<IntegrityReport> {
    if (this.opts.verifyError) throw new Error('verify boom');
    return report;
  }
  async health(): Promise<boolean> {
    return true;
  }
}

let app: ReturnType<typeof buildServer>;
function start(events: AuditEvent[], overrides: Partial<Config> = {}, opts: { verifyError?: boolean } = {}) {
  app = buildServer({ ...cfg, ...overrides }, { daemon: new FakeDaemon(events, opts) });
  return app;
}
afterEach(async () => {
  await app?.close();
});

const check = (id: string, ms: number) => ev(id, ms, 'demo', { type: 'capability_check', passed: true, agent_id: 'm', required_actions: [], missing_actions: [] });

describe('auth', () => {
  it('rejects missing bearer', async () => {
    start([]);
    expect((await app.inject({ method: 'GET', url: '/v1/conduct-events' })).statusCode).toBe(401);
  });
  it('rejects wrong token', async () => {
    start([]);
    expect((await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: { authorization: 'Bearer nope' } })).statusCode).toBe(403);
  });
  it('rejects a same-length wrong token', async () => {
    start([]);
    // 'Bearer nope' above is a different length, so requireFairscale short-circuits
    // on the length check and never reaches timingSafeEqual. Flip one byte at equal
    // length so the constant-time compare is the only thing that can reject it.
    const last = cfg.apiToken.slice(-1);
    const sameLengthWrong = `${cfg.apiToken.slice(0, -1)}${last === 'x' ? 'y' : 'x'}`;
    expect(sameLengthWrong).toHaveLength(cfg.apiToken.length);
    expect(sameLengthWrong).not.toBe(cfg.apiToken);
    const res = await app.inject({
      method: 'GET',
      url: '/v1/conduct-events',
      headers: { authorization: `Bearer ${sameLengthWrong}` },
    });
    expect(res.statusCode).toBe(403);
    expect(res.json()).toMatchObject({ error: 'invalid token' });
  });
  it('leaves healthz public with version', async () => {
    start([]);
    const res = await app.inject({ method: 'GET', url: '/healthz' });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ ok: true, service: 'fairscale-bridge', version: '1' });
  });
});

describe('conduct-events', () => {
  it('returns mapped events, version, no-store, and attestation envelope', async () => {
    start([check('a', 1000), ev('b', 2000, 'demo', { type: 'authentication_failed', transport: 'http', reason: 'bad' })]);
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: auth });
    expect(res.statusCode).toBe(200);
    expect(res.headers['cache-control']).toBe('no-store');
    const body = res.json();
    expect(body).toMatchObject({ version: '1', pillar: 'work_history', count: 2, attested: true });
    expect(body.attestation).toMatchObject({ audit_root: 'deadbeef', verified: true });
    expect(body.events[0].occurred_at_ms).toBeLessThan(body.events[1].occurred_at_ms);
  });

  it('paginates exactly across a shared-timestamp boundary with no overlap or gap', async () => {
    start([check('a', 1000), check('b', 2000), check('c', 2000), check('d', 3000)]);
    const seen: string[] = [];
    let url = '/v1/conduct-events?limit=2';
    for (let i = 0; i < 5; i++) {
      const body = (await app.inject({ method: 'GET', url, headers: auth })).json();
      seen.push(...body.events.map((e: { id: string }) => e.id));
      if (!body.has_more) break;
      url = `/v1/conduct-events?cursor=${encodeURIComponent(body.next_cursor)}&limit=2`;
    }
    expect(seen).toEqual(['a', 'b', 'c', 'd']);
    expect(new Set(seen).size).toBe(4);
  });

  it('reports a resumable cursor when caught up', async () => {
    start([check('a', 1000)]);
    const first = (await app.inject({ method: 'GET', url: '/v1/conduct-events?limit=10', headers: auth })).json();
    expect(first.has_more).toBe(false);
    expect(first.next_cursor).not.toBeNull();
    const next = (await app.inject({ method: 'GET', url: `/v1/conduct-events?cursor=${encodeURIComponent(first.next_cursor)}`, headers: auth })).json();
    expect(next.count).toBe(0);
    expect(next.next_cursor).toBe(first.next_cursor);
  });

  it('returns null next_cursor on an empty stream from the start', async () => {
    start([]);
    const body = (await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: auth })).json();
    expect(body.count).toBe(0);
    expect(body.next_cursor).toBeNull();
    expect(body.has_more).toBe(false);
  });

  it('scopes to a single agent by pubkey', async () => {
    start([check('a', 1000), ev('b', 2000, 'other', { type: 'capability_check', passed: true, agent_id: 'x', required_actions: [], missing_actions: [] })]);
    const body = (await app.inject({ method: 'GET', url: '/v1/agents/pk_demo/conduct-events', headers: auth })).json();
    expect(body.agent_scope).toBe('pk_demo');
    expect(body.count).toBe(1);
    expect(body.events.every((e: { agent_id: string }) => e.agent_id === 'pk_demo')).toBe(true);
  });

  it('flags truncation when the daemon fetch hits the cap', async () => {
    start([check('a', 1000), check('b', 2000), check('c', 3000)], { fetchCap: 2 });
    const body = (await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: auth })).json();
    expect(body.truncated).toBe(true);
  });

  it('serves events even when attestation is unavailable', async () => {
    start([check('a', 1000)], {}, { verifyError: true });
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: auth });
    expect(res.statusCode).toBe(200);
    const body = res.json();
    expect(body.count).toBe(1);
    expect(body.attested).toBe(false);
    expect(body.attestation).toBeNull();
  });

  it('rejects a malformed cursor', async () => {
    start([]);
    expect((await app.inject({ method: 'GET', url: '/v1/conduct-events?cursor=!!bad!!', headers: auth })).statusCode).toBe(400);
  });

  it('rejects a malformed since', async () => {
    start([]);
    expect((await app.inject({ method: 'GET', url: '/v1/conduct-events?since=notadate', headers: auth })).statusCode).toBe(400);
  });
});

describe('attestation endpoint', () => {
  it('exposes the verified audit root', async () => {
    start([]);
    const res = await app.inject({ method: 'GET', url: '/v1/attestation', headers: auth });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ audit_root: 'deadbeef', verified: true, event_count: 4 });
  });

  it('is strict: 502 when the daemon verify fails', async () => {
    start([], {}, { verifyError: true });
    expect((await app.inject({ method: 'GET', url: '/v1/attestation', headers: auth })).statusCode).toBe(502);
  });
});
