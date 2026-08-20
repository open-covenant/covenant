import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { SignJWT } from 'jose';

// The authenticated /prove arms sit behind a Redis-backed rate limiter whose
// decision is a Lua eval the route reads as [allowed, ttl], plus a cache
// lookup. ioredis-mock can't run that Lua script (the rate-limit unit test
// hand-rolls a fake conn for the same reason), so drive both through a
// controllable connection: each test sets evalResult to allow, deny, or throw.
const ctl = vi.hoisted(() => ({ evalResult: [1, 0] as [number, number] | Error }));

vi.mock('../queue.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../queue.js')>();
  return {
    ...actual,
    redisConnection: () => ({
      eval: async () => {
        if (ctl.evalResult instanceof Error) throw ctl.evalResult;
        return ctl.evalResult;
      },
      get: async () => null,
      ping: async () => 'PONG',
      disconnect: () => undefined,
    }),
    buildQueue: () => ({
      getWaitingCount: async () => 0,
      getActiveCount: async () => 0,
      getFailedCount: async () => 0,
      getJob: async () => null,
      add: async () => undefined,
      close: async () => undefined,
    }),
  };
});

const SESSION_SECRET = 'x'.repeat(40);

const withArtifactsDir = (): string => {
  const dir = mkdtempSync(join(tmpdir(), 'proofgen-prove-'));
  mkdirSync(join(dir, 'task_completion_js'), { recursive: true });
  writeFileSync(join(dir, 'task_completion_js', 'task_completion.wasm'), '');
  writeFileSync(join(dir, 'task_completion.zkey'), '');
  return dir;
};

const mintToken = (agentDid: string): Promise<string> =>
  new SignJWT({})
    .setProtectedHeader({ alg: 'HS256' })
    .setIssuer('covenant.portal')
    .setAudience('covenant:proof-gen')
    .setSubject(agentDid)
    .setExpirationTime('5m')
    .sign(new TextEncoder().encode(SESSION_SECRET));

const loadServer = async () => {
  process.env.SESSION_SECRET = SESSION_SECRET;
  process.env.CIRCUIT_ARTIFACTS_DIR = withArtifactsDir();
  vi.resetModules();
  const { buildServer } = await import('../server.js');
  return buildServer();
};

describe('proof-gen /prove authenticated rate-limit + body arms', () => {
  let handles: Awaited<ReturnType<typeof loadServer>>;
  let auth: { authorization: string };
  beforeAll(async () => {
    handles = await loadServer();
    await handles.app.ready();
    auth = { authorization: `Bearer ${await mintToken('did:agent:prove')}` };
  });
  afterAll(async () => {
    await handles.close();
  });

  it('returns 429 rate_limited when the limiter denies the agent', async () => {
    ctl.evalResult = [0, 2500];
    const res = await handles.app.inject({ method: 'POST', url: '/prove', headers: auth, payload: {} });
    expect(res.statusCode).toBe(429);
    expect(res.json()).toMatchObject({ error: 'rate_limited', retry_after: 3 });
  });

  it('fails closed with 503 when the rate-limit backend errors', async () => {
    ctl.evalResult = new Error('ECONNREFUSED');
    const res = await handles.app.inject({ method: 'POST', url: '/prove', headers: auth, payload: {} });
    expect(res.statusCode).toBe(503);
    expect(res.json()).toMatchObject({ error: 'rate_limit_backend_unavailable' });
  });

  it('rejects an incomplete body with 400 once authenticated and under the limit', async () => {
    ctl.evalResult = [1, 0];
    const res = await handles.app.inject({
      method: 'POST',
      url: '/prove',
      headers: auth,
      payload: { circuit_id: 'task_completion.v1' },
    });
    expect(res.statusCode).toBe(400);
    expect(res.json()).toMatchObject({ error: 'invalid_body' });
  });
});
