import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

// Replace the BullMQ queue + ioredis connection with in-memory fakes so the
// route handlers can be exercised without a live Redis. getJob returns null so
// /jobs/:id falls through to its 404 arm; the RedisMock connection returns null
// for any unknown cache/result key.
vi.mock('../queue.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../queue.js')>();
  const { default: RedisMock } = await import('ioredis-mock');
  return {
    ...actual,
    redisConnection: () => new RedisMock(),
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

const freshArtifactsDir = (withArtifacts: boolean): string => {
  const dir = mkdtempSync(join(tmpdir(), 'proofgen-'));
  if (withArtifacts) {
    mkdirSync(join(dir, 'task_completion_js'), { recursive: true });
    writeFileSync(join(dir, 'task_completion_js', 'task_completion.wasm'), '');
    writeFileSync(join(dir, 'task_completion.zkey'), '');
  }
  return dir;
};

const loadServer = async (withArtifacts: boolean) => {
  process.env.CIRCUIT_ARTIFACTS_DIR = freshArtifactsDir(withArtifacts);
  vi.resetModules();
  const { buildServer } = await import('../server.js');
  return buildServer();
};

describe('proof-gen server without circuit artifacts', () => {
  let handles: Awaited<ReturnType<typeof loadServer>>;
  beforeAll(async () => {
    handles = await loadServer(false);
    await handles.app.ready();
  });
  afterAll(async () => {
    await handles.close();
  });

  it('POST /prove fails closed with 503 when artifacts are missing', async () => {
    const res = await handles.app.inject({ method: 'POST', url: '/prove', payload: {} });
    expect(res.statusCode).toBe(503);
    expect(res.json()).toMatchObject({ error: 'no_artifacts' });
  });

  it('GET /jobs/:id rejects a non-uuid id with 400', async () => {
    const res = await handles.app.inject({ method: 'GET', url: '/jobs/not-a-uuid' });
    expect(res.statusCode).toBe(400);
    expect(res.json()).toMatchObject({ error: 'invalid_id' });
  });

  it('GET /jobs/:id returns 404 for an unknown job', async () => {
    const res = await handles.app.inject({ method: 'GET', url: `/jobs/${randomUUID()}` });
    expect(res.statusCode).toBe(404);
    expect(res.json()).toMatchObject({ error: 'not_found' });
  });
});

describe('proof-gen server with circuit artifacts present', () => {
  let handles: Awaited<ReturnType<typeof loadServer>>;
  beforeAll(async () => {
    handles = await loadServer(true);
    await handles.app.ready();
  });
  afterAll(async () => {
    await handles.close();
  });

  it('POST /prove fails closed with 401 when no bearer token is supplied', async () => {
    const res = await handles.app.inject({ method: 'POST', url: '/prove', payload: {} });
    expect(res.statusCode).toBe(401);
    expect(res.json()).toMatchObject({ error: 'unauthorized' });
  });

  it('POST /prove rejects a non-Bearer authorization scheme with 401', async () => {
    const res = await handles.app.inject({
      method: 'POST',
      url: '/prove',
      headers: { authorization: 'Basic abc' },
      payload: {},
    });
    expect(res.statusCode).toBe(401);
    expect(res.json()).toMatchObject({ error: 'unauthorized' });
  });
});
