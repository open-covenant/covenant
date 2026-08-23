import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  assertOperatorControlOpen,
  createApp,
  OperatorAdmissionError,
  SerialGate,
  type AppDependencies,
} from './app.js';
import { MemoryStore } from './store.js';
import type { Quote } from './types.js';

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map(
      (server) =>
        new Promise<void>((resolve, reject) => {
          server.close((cause) => (cause ? reject(cause) : resolve()));
        }),
    ),
  );
});

describe('operator admission controls', () => {
  it('requires authentication to open intake and exposes a public status', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        config: { adminToken: 'admin-secret', paymentMode: 'mock' },
      }),
    );

    const initial = await fetch(`${base}/v1/admission`);
    await expect(initial.json()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });

    const body = JSON.stringify({
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'canary checks completed successfully',
    });
    const unauthorized = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body,
    });
    expect(unauthorized.status).toBe(401);

    const opened = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body,
    });
    expect(opened.status).toBe(200);
    await expect(opened.json()).resolves.toMatchObject({
      intakeEnabled: true,
      claimsEnabled: true,
      revision: 1,
      updatedBy: 'operator',
    });
  });

  it('keeps controls closed while service dependencies are unavailable', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body: JSON.stringify({
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'attempted canary while dependencies are unavailable',
      }),
    });

    expect(response.status).toBe(503);
    await expect(store.operatorControls()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });
  });

  it('does not issue a quote when stale controls are open but readiness is incomplete', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'simulate stale controls from an earlier deployment',
      updatedBy: 'test',
    });
    const issue = vi.fn();
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: { issue },
        payments: { challenge },
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/2' }),
    });

    expect(response.status).toBe(503);
    expect(issue).not.toHaveBeenCalled();
    expect(challenge).not.toHaveBeenCalled();
  });

  it('fails closed before settlement while preserving authoritative idempotent reads', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const settle = vi.fn();
    const deps = dependencies(store, {
      config: { adminToken: 'admin-secret', paymentMode: 'mock' },
      payments: { settle },
      github: {
        assertIssueAuthorization: vi.fn(async () => undefined),
        currentHead: vi.fn(async () => quote.baseSha),
      },
    });
    const base = await serve(deps);

    const blocked = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'job-key' },
      body: JSON.stringify({ quote_id: quote.id }),
    });
    expect(blocked.status).toBe(503);
    await expect(blocked.json()).resolves.toEqual({ error: 'intake is paused by the operator' });
    expect(settle).not.toHaveBeenCalled();

    const reservation = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'pending', amountAtomic: quote.priceAtomic },
      'job-key',
    );
    const replay = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'job-key' },
      body: JSON.stringify({ quote_id: quote.id }),
    });
    expect(replay.status).toBe(200);
    await expect(replay.json()).resolves.toMatchObject({ id: reservation.job.id });
    expect(settle).not.toHaveBeenCalled();
  });

  it('does not accept payment when stale intake is open but readiness is incomplete', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'simulate stale intake from an earlier deployment',
      updatedBy: 'test',
    });
    const settle = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'unready-job' },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(503);
    expect(settle).not.toHaveBeenCalled();
    await expect(store.jobByIdempotencyKey('unready-job')).resolves.toBeUndefined();
  });

  it('treats an unavailable durable control row as closed', async () => {
    const store = {
      operatorControls: vi.fn(async () => {
        throw new Error('database unavailable');
      }),
    };
    await expect(
      assertOperatorControlOpen(store as unknown as MemoryStore, 'intake'),
    ).rejects.toBeInstanceOf(OperatorAdmissionError);
  });

  it('recovers an existing settlement while new intake remains closed', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const { job } = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'pending', amountAtomic: quote.priceAtomic },
      'recovery-key',
    );
    const retrySettlement = vi.fn(async () => ({
      payer: 'payer',
      transaction: 'settled-transaction',
      amountAtomic: quote.priceAtomic,
    }));
    const base = await serve(
      dependencies(store, {
        payments: { retrySettlement },
        processor: { process: vi.fn() },
      }),
    );
    const request = () =>
      fetch(`${base}/v1/admin/settlements/${job.id}`, {
        method: 'POST',
        headers: { authorization: 'Bearer admin-secret' },
      });

    const resumed = await request();
    expect(resumed.status).toBe(202);
    expect(retrySettlement).toHaveBeenCalledTimes(1);
    await expect(store.operatorControls()).resolves.toMatchObject({ intakeEnabled: false });
    await expect(store.job(job.id)).resolves.toMatchObject({
      id: job.id,
      state: 'paid',
      payment: { transaction: 'settled-transaction' },
    });
  });
});

describe('public route responses', () => {
  it('rejects feature work before issuing a payment challenge', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'scope validation test intake',
      updatedBy: 'test',
    });
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => ({
            owner: 'example',
            repo: 'project',
            number: 2,
            title: 'Add a reset button',
            body: 'Expose a new UI control.',
            labels: ['enhancement'],
            defaultBranch: 'main',
            baseSha: 'a'.repeat(40),
            rootFiles: ['package.json', 'pnpm-lock.yaml'],
          })),
        },
        payments: { challenge },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/2' }),
    });

    expect(response.status).toBe(422);
    await expect(response.json()).resolves.toEqual({
      error: "issue labels place it outside Mizuki's maintenance-only scope",
    });
    expect(challenge).not.toHaveBeenCalled();
  });

  it('rejects issue drift before settlement', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const settle = vi.fn();
    const assertIssueAuthorization = vi.fn(async () => {
      throw new Error('GitHub issue changed after the quote; request a new quote');
    });
    const base = await serve(
      dependencies(store, {
        github: { assertIssueAuthorization },
        payments: { settle },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'drifted-job' },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: 'GitHub issue changed after the quote; request a new quote',
    });
    expect(assertIssueAuthorization).toHaveBeenCalledWith(
      quote.owner,
      quote.repo,
      quote.issueNumber,
      quote.installationId,
      quote.authorizationReceipt?.evidenceHash,
      { title: quote.issueTitle, body: quote.issueBody },
    );
    expect(settle).not.toHaveBeenCalled();
  });

  it('returns 429 with Retry-After when a source exceeds the OAuth bucket', async () => {
    const store = new MemoryStore();
    const authorizeUrl = vi.fn(() => 'https://github.com/login/oauth/authorize');
    const base = await serve(dependencies(store, { auth: { authorizeUrl } }));

    for (let index = 0; index < 10; index += 1) {
      const response = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
      expect(response.status).toBe(302);
    }
    const limited = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
    expect(limited.status).toBe(429);
    expect(limited.headers.get('retry-after')).toBe('6');
    expect(authorizeUrl).toHaveBeenCalledTimes(10);
  });

  it('sets a Secure OAuth session cookie from an authenticated HTTPS web request', async () => {
    const store = new MemoryStore();
    const proxySecret = 'p'.repeat(32);
    const base = await serve(
      dependencies(store, {
        config: {
          publicBaseUrl: 'https://mizuki-api.onrender.com',
          webOrigin: 'https://mizuki.covenant.org',
          trustedProxyHops: 1,
          webProxySecret: proxySecret,
        },
        auth: {
          callback: vi.fn(async () => ({ session: 'signed-session', redirect: '/bounties' })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/auth/github/callback?code=code&state=state`, {
      headers: {
        'x-forwarded-proto': 'http',
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': proxySecret,
      },
      redirect: 'manual',
    });

    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('https://mizuki.covenant.org/bounties');
    expect(response.headers.get('set-cookie')).toContain('; Secure');
  });

  it('closes an activity stream after its configured idle lifetime', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        config: { sseIdleTimeoutMs: 25 },
      }),
    );
    const startedAt = Date.now();
    const response = await fetch(`${base}/v1/events`);
    expect(response.status).toBe(200);
    await response.text();
    expect(Date.now() - startedAt).toBeLessThan(1_000);
  });
});

async function serve(deps: AppDependencies): Promise<string> {
  const server = createServer(createApp(deps));
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('test server did not bind');
  return `http://127.0.0.1:${address.port}`;
}

function dependencies(
  store: MemoryStore,
  overrides: Record<string, unknown> = {},
): AppDependencies {
  const { config: configOverride, ...dependencyOverrides } = overrides;
  return {
    config: {
      adminToken: 'admin-secret',
      paymentMode: 'mock',
      trustedProxyHops: 0,
      rateLimitMaxSources: 100,
      sseMaxConnections: 10,
      sseMaxConnectionsPerSource: 2,
      sseIdleTimeoutMs: 10_000,
      ...(configOverride as object | undefined),
    },
    store,
    github: {},
    payments: {},
    processor: {},
    auth: {},
    webhooks: {},
    bounties: {},
    policy: {},
    paymentAdmission: new SerialGate(),
    readiness: { check: vi.fn(async () => ({ ready: true })) },
    ...dependencyOverrides,
  } as unknown as AppDependencies;
}

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl: 'https://github.com/example/project/issues/1',
  owner: 'example',
  repo: 'project',
  issueNumber: 1,
  issueTitle: 'Fix docs',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: [],
  expiresAt: '2099-01-01T00:00:00.000Z',
};
