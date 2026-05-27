import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { buildServer } from './server.js';
import type { Config } from './config.js';
import type { AuditEvent, DaemonClient, IntegrityReport } from './daemon.js';

const cfg: Config = {
  port: 0,
  daemonUrl: 'http://daemon',
  daemonToken: 'op-token',
  apiToken: 'fairscale-test-token-0123456789',
  maxLimit: 1000,
  defaultLimit: 100,
};

const auth = { authorization: `Bearer ${cfg.apiToken}` };

function ev(id: string, ms: number, issuer: string, kind: AuditEvent['kind']): AuditEvent {
  return { id, timestamp_ms: ms, issuer: { display: issuer, pubkey: `pk_${issuer}` }, kind };
}

class FakeDaemon implements DaemonClient {
  constructor(private events: AuditEvent[], private report: IntegrityReport) {}
  async recentAudit({ sinceMs, limit }: { sinceMs?: number; limit: number }): Promise<AuditEvent[]> {
    let out = [...this.events].sort((a, b) => a.timestamp_ms - b.timestamp_ms);
    if (sinceMs !== undefined) out = out.filter((e) => e.timestamp_ms >= sinceMs);
    return out.slice(Math.max(0, out.length - limit));
  }
  async verify(): Promise<IntegrityReport> {
    return this.report;
  }
  async health(): Promise<boolean> {
    return true;
  }
}

const report: IntegrityReport = { events: 3, anchors: 1, valid: true, root_hash_hex: 'deadbeef', failures: [] };

let app: ReturnType<typeof buildServer>;

function start(events: AuditEvent[]) {
  app = buildServer(cfg, { daemon: new FakeDaemon(events, report) });
  return app;
}

afterEach(async () => {
  await app?.close();
});

describe('auth', () => {
  beforeEach(() => start([]));

  it('rejects missing bearer', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events' });
    expect(res.statusCode).toBe(401);
  });

  it('rejects wrong token', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: { authorization: 'Bearer nope' } });
    expect(res.statusCode).toBe(403);
  });

  it('leaves healthz public', async () => {
    const res = await app.inject({ method: 'GET', url: '/healthz' });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ ok: true, service: 'fairscale-bridge' });
  });
});

describe('conduct-events', () => {
  const events = [
    ev('a', 1000, 'demo', { type: 'intent_dispatched', status: 'success', intent_text: 'one' }),
    ev('b', 2000, 'demo', { type: 'authentication_failed', transport: 'http', reason: 'bad' }),
    ev('c', 3000, 'other', { type: 'capability_check', passed: true, agent_id: 'x', required_actions: [], missing_actions: [] }),
  ];
  beforeEach(() => start(events));

  it('returns mapped events with attestation envelope', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events', headers: auth });
    expect(res.statusCode).toBe(200);
    const body = res.json();
    expect(body.pillar).toBe('work_history');
    expect(body.count).toBe(3);
    expect(body.attestation).toMatchObject({ audit_root: 'deadbeef', verified: true });
    expect(body.events[0].occurred_at_ms).toBeLessThan(body.events[1].occurred_at_ms);
  });

  it('paginates with a cursor and reports has_more', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events?since=0&limit=2', headers: auth });
    const body = res.json();
    expect(body.count).toBe(2);
    expect(body.has_more).toBe(true);
    expect(body.next_cursor).toBe(2000);

    const next = await app.inject({ method: 'GET', url: `/v1/conduct-events?cursor=${body.next_cursor}&limit=2`, headers: auth });
    const nb = next.json();
    expect(nb.events.map((e: { id: string }) => e.id)).toContain('c');
  });

  it('scopes to a single agent by pubkey or display', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/agents/pk_demo/conduct-events?since=0', headers: auth });
    const body = res.json();
    expect(body.agent_scope).toBe('pk_demo');
    expect(body.events.every((e: { agent_id: string }) => e.agent_id === 'pk_demo')).toBe(true);
    expect(body.count).toBe(2);
  });

  it('rejects a malformed since', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/conduct-events?since=notadate', headers: auth });
    expect(res.statusCode).toBe(400);
  });
});

describe('attestation', () => {
  beforeEach(() => start([]));

  it('exposes the verified audit root', async () => {
    const res = await app.inject({ method: 'GET', url: '/v1/attestation', headers: auth });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toMatchObject({ audit_root: 'deadbeef', verified: true, event_count: 3 });
  });
});
