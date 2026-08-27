import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, SerialGate, type AppDependencies } from './app.js';
import { ApiTokenAuthError } from './auth.js';
import { GithubReadinessError } from './github.js';
import { PolicyRequestError } from './policy-client.js';
import { MemoryStore } from './store.js';
import type { GithubIssue, Payment, Quote } from './types.js';

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
  vi.restoreAllMocks();
});

describe('workbench account API', () => {
  it('creates, lists, and revokes one-time scoped API tokens through the browser session', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const base = await serve(dependencies(store));

    const csrfResponse = await fetch(`${base}/v1/auth/csrf`, { headers: sessionHeaders });
    expect(csrfResponse.status).toBe(200);
    expect(csrfResponse.headers.get('cache-control')).toBe('private, no-store');
    const csrf = (await csrfResponse.json()) as { csrfToken: string };

    const created = await fetch(`${base}/v1/account/api-tokens`, {
      method: 'POST',
      headers: {
        ...sessionHeaders,
        'content-type': 'application/json',
        'x-mizuki-csrf-token': csrf.csrfToken,
      },
      body: JSON.stringify({
        name: 'Release MCP',
        scopes: ['repositories:read', 'jobs:read'],
        expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
      }),
    });
    expect(created.status).toBe(201);
    expect(created.headers.get('cache-control')).toBe('private, no-store');
    const credential = (await created.json()) as {
      secret: string;
      token: { id: string; prefix: string; state: string };
    };
    expect(credential.secret).toMatch(/^mzk_v1_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/);
    expect(credential.token).toMatchObject({ state: 'active' });
    const stored = await store.apiTokensForAccount('42');
    expect(stored).toHaveLength(1);
    expect(stored[0]?.tokenHash).toMatch(/^[a-f0-9]{64}$/);
    expect(JSON.stringify(stored)).not.toContain(credential.secret);

    const listed = await fetch(`${base}/v1/account/api-tokens`, { headers: sessionHeaders });
    const listBody = (await listed.json()) as Record<string, unknown>;
    expect(listBody).toMatchObject({
      tokens: [
        {
          id: credential.token.id,
          name: 'Release MCP',
          scopes: ['repositories:read', 'jobs:read'],
          state: 'active',
        },
      ],
    });
    expect(JSON.stringify(listBody)).not.toContain(credential.secret);
    expect(JSON.stringify(listBody)).not.toContain(stored[0]!.tokenHash);

    const bearerCannotMint = await fetch(`${base}/v1/account/api-tokens`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${credential.secret}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({
        name: 'Nested token',
        scopes: ['jobs:read'],
        expiresAt: new Date(Date.now() + 24 * 60 * 60_000).toISOString(),
      }),
    });
    expect(bearerCannotMint.status).toBe(401);
    const bearerCannotList = await fetch(`${base}/v1/account/api-tokens`, {
      headers: { authorization: `Bearer ${credential.secret}` },
    });
    expect(bearerCannotList.status).toBe(401);

    const revoked = await fetch(`${base}/v1/account/api-tokens/${credential.token.id}/revoke`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'x-mizuki-csrf-token': csrf.csrfToken },
    });
    expect(revoked.status).toBe(200);
    const revokedBody = await revoked.json();
    expect(revokedBody).toMatchObject({ token: { state: 'revoked' } });
    expect(JSON.stringify(revokedBody)).not.toContain(credential.secret);
  });

  it('rejects token mutations without a session-bound CSRF token and exact browser origin', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const base = await serve(dependencies(store));
    const input = {
      name: 'Release MCP',
      scopes: ['repositories:read'],
      expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
    };

    const missingToken = await fetch(`${base}/v1/account/api-tokens`, {
      method: 'POST',
      headers: {
        cookie: sessionHeaders.cookie,
        origin: sessionHeaders.origin,
        'content-type': 'application/json',
      },
      body: JSON.stringify(input),
    });
    expect(missingToken.status).toBe(403);
    await expect(missingToken.json()).resolves.toEqual({ error: 'CSRF validation failed' });

    const wrongOrigin = await fetch(`${base}/v1/account/api-tokens`, {
      method: 'POST',
      headers: {
        ...sessionHeaders,
        origin: 'https://attacker.example',
        'content-type': 'application/json',
        'x-mizuki-csrf-token': 'c'.repeat(43),
      },
      body: JSON.stringify(input),
    });
    expect(wrongOrigin.status).toBe(403);

    const invalidBody = await fetch(`${base}/v1/account/api-tokens`, {
      method: 'POST',
      headers: {
        ...sessionHeaders,
        'content-type': 'application/json',
        'x-mizuki-csrf-token': 'c'.repeat(43),
      },
      body: 'null',
    });
    expect(invalidBody.status).toBe(400);
    expect(await store.apiTokensForAccount('42')).toEqual([]);
  });

  it('accepts scoped bearer access on MCP-safe routes without a browser cookie', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    const apiToken = vi.fn(async (value: string, scope: string) => ({
      kind: 'api_token' as const,
      tokenId: '11111111-1111-4111-8111-111111111111',
      githubId: '42',
      githubLogin: 'maintainer',
      scopes: [scope],
    }));
    const base = await serve(
      dependencies(store, {
        auth: { session: vi.fn(), apiToken },
        github: {
          repositoryMetadataForMaintainer: vi.fn(async () => repositoryMetadata),
        },
        policy: {
          assertRepositoryReady: vi.fn(async () => ({ verifierAppId: '20', installationId: 30 })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/account/repositories`, {
      headers: { authorization: 'Bearer mzk_v1_machine-credential' },
    });

    expect(response.status).toBe(200);
    expect(apiToken).toHaveBeenCalledWith('mzk_v1_machine-credential', 'repositories:read');
    await expect(response.json()).resolves.toMatchObject({
      repositories: [{ repository: 'example/project' }],
    });
  });

  it('returns a redacted forbidden response when an API token lacks a route scope', async () => {
    const store = new MemoryStore();
    const secret = 'mzk_v1_sensitive-machine-token';
    const base = await serve(
      dependencies(store, {
        auth: {
          session: vi.fn(),
          apiToken: vi.fn(async () => {
            throw new ApiTokenAuthError('insufficient_scope');
          }),
        },
      }),
    );

    const response = await fetch(`${base}/v1/preflights`, {
      method: 'POST',
      headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(response.status).toBe(403);
    const body = await response.text();
    expect(body).toContain('required scope');
    expect(body).not.toContain(secret);
  });

  it.each([
    {
      route: 'account quote creation',
      path: '/v1/account/quotes',
      scope: 'jobs:write',
      init: {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: issueUrl }),
      },
    },
    {
      route: 'payment recovery',
      path: `/v1/account/quotes/${quote.id}/payment-status`,
      scope: 'jobs:read',
      init: { headers: { 'idempotency-key': 'scope-matrix-payment' } },
    },
  ])('requires the $scope scope for $route', async ({ path, scope, init }) => {
    const apiToken = vi.fn(async () => {
      throw new ApiTokenAuthError('insufficient_scope');
    });
    const base = await serve(
      dependencies(new MemoryStore(), {
        auth: { session: vi.fn(), apiToken },
      }),
    );

    const response = await fetch(`${base}${path}`, {
      ...init,
      headers: {
        ...init.headers,
        authorization: 'Bearer mzk_v1_scope-matrix',
      },
    });

    expect(response.status).toBe(403);
    expect(apiToken).toHaveBeenCalledWith('mzk_v1_scope-matrix', scope);
  });

  it('keeps repository and policy readiness independent during a GitHub outage', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    const base = await serve(
      dependencies(store, {
        github: {
          repositoryMetadataForMaintainer: vi.fn(async () => {
            throw new GithubReadinessError(
              'unavailable',
              'GitHub repository access is temporarily unavailable. Please try again shortly.',
              429,
            );
          }),
        },
        policy: {
          assertRepositoryReady: vi.fn(async () => ({ verifierAppId: '20', installationId: 30 })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/account/repositories`, {
      headers: sessionHeaders,
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('x-request-id')).toBeTruthy();
    await expect(response.json()).resolves.toMatchObject({
      repositories: [
        {
          repository: 'example/project',
          core: { status: 'unavailable' },
          policy: { status: 'ready' },
          readiness: 'unavailable',
          readyForWork: false,
          blockers: [
            'GitHub repository access is temporarily unavailable. Please try again shortly.',
          ],
        },
      ],
    });
  });

  it('does not report a policy signer outage as a missing installation', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    const base = await serve(
      dependencies(store, {
        github: {
          repositoryMetadataForMaintainer: vi.fn(async () => repositoryMetadata),
        },
        policy: {
          assertRepositoryReady: vi.fn(async () => {
            throw new Error('private signer detail');
          }),
        },
      }),
    );

    const response = await fetch(`${base}/v1/account/repositories`, {
      headers: sessionHeaders,
    });

    expect(response.status).toBe(200);
    const body = await response.json();
    expect(body).toMatchObject({
      repositories: [
        {
          core: { status: 'ready' },
          policy: {
            status: 'unavailable',
            reason: 'The read-only policy verifier is temporarily unavailable.',
          },
          readiness: 'unavailable',
          readyForWork: false,
          blockers: ['The read-only policy verifier is temporarily unavailable.'],
        },
      ],
    });
    expect(JSON.stringify(body)).not.toContain('private signer detail');
  });

  it('returns only the signed-in account jobs and real settlement totals', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'account-job');
    await store.transitionJob(job.id, 'settlement_pending', 'paid');
    const base = await serve(dependencies(store));

    const account = await fetch(`${base}/v1/account`, { headers: sessionHeaders });
    expect(account.status).toBe(200);
    await expect(account.json()).resolves.toEqual({
      account: { githubId: '42', githubLogin: 'maintainer' },
    });

    const jobs = await fetch(`${base}/v1/account/jobs`, { headers: sessionHeaders });
    await expect(jobs.json()).resolves.toMatchObject({
      jobs: [{ id: job.id, state: 'paid' }],
      limit: 100,
      truncated: false,
      obligationCount: 1,
    });

    const billing = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });
    await expect(billing.json()).resolves.toMatchObject({
      mode: 'mock',
      asset: 'USDC',
      limit: 1000,
      truncated: false,
      totalsScope: 'account_lifetime',
      totals: {
        confirmingAtomic: '0',
        paidAtomic: '2000000',
        refundedAtomic: '0',
        deliveredAtomic: '0',
        protectedAtomic: '2000000',
      },
      transactions: [{ jobId: job.id, type: 'payment', transaction: 'settlement' }],
    });

    const anonymous = await fetch(`${base}/v1/account/jobs`);
    expect(anonymous.status).toBe(401);
  });

  it('lists connected repository pull requests with truthful job provenance', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'account-job');
    await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      prUrl: 'https://github.com/example/project/pull/185',
    });
    const pullRequestsForMaintainer = vi.fn(async () => ({
      repository: repositoryMetadata,
      pullRequests: [
        {
          number: 196,
          title: 'Add the RWA firewall',
          url: 'https://github.com/example/project/pull/196',
          state: 'open' as const,
          draft: false,
          authorized: true,
          author: 'contributor',
          headRef: 'feat/rwa-firewall',
          headSha: 'b'.repeat(40),
          baseRef: 'main',
          createdAt: '2026-08-26T06:43:34.000Z',
          updatedAt: '2026-08-26T12:00:00.000Z',
        },
        {
          number: 185,
          title: 'Delivered maintenance patch',
          url: 'https://github.com/example/project/pull/185',
          state: 'merged' as const,
          draft: false,
          authorized: false,
          author: 'mizuki0x',
          headRef: 'mizuki/job',
          headSha: 'c'.repeat(40),
          baseRef: 'main',
          createdAt: '2026-08-25T06:43:34.000Z',
          updatedAt: '2026-08-25T12:00:00.000Z',
        },
      ],
    }));
    const base = await serve(dependencies(store, { github: { pullRequestsForMaintainer } }));

    const response = await fetch(`${base}/v1/account/pull-requests`, {
      headers: sessionHeaders,
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('private, no-store');
    await expect(response.json()).resolves.toMatchObject({
      pullRequests: [
        { number: 196, authorized: true, provenance: { kind: 'unlinked' } },
        {
          number: 185,
          provenance: { kind: 'paid_job', jobId: job.id, state: 'delivered' },
        },
      ],
      truncated: false,
      unavailableRepositories: [],
    });
    expect(pullRequestsForMaintainer).toHaveBeenCalledWith('example', 'project', 'maintainer');

    const anonymous = await fetch(`${base}/v1/account/pull-requests`);
    expect(anonymous.status).toBe(401);
  });

  it('keeps delivered work protected until its refund liability is discharged', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'delivered-liability');
    await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      refundLiabilityId: 'liability-1',
    });
    const base = await serve(dependencies(store));

    const protectedBilling = await fetch(`${base}/v1/account/billing`, {
      headers: sessionHeaders,
    });
    await expect(protectedBilling.json()).resolves.toMatchObject({
      obligationCount: 1,
      totals: {
        deliveredAtomic: '2000000',
        protectedAtomic: '2000000',
      },
    });

    await store.patchJob(job.id, {
      refundLiabilityDischargedAt: '2026-08-25T05:00:00.000Z',
    });
    const dischargedBilling = await fetch(`${base}/v1/account/billing`, {
      headers: sessionHeaders,
    });
    await expect(dischargedBilling.json()).resolves.toMatchObject({
      obligationCount: 0,
      totals: {
        deliveredAtomic: '2000000',
        protectedAtomic: '0',
      },
    });
  });

  it('checks an unpaid quote without requesting payment or repository access', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const settle = vi.fn();
    const github = {
      assertIssueAuthorization: vi.fn(),
      currentHead: vi.fn(),
    };
    const base = await serve(dependencies(store, { payments: { settle }, github }));

    const response = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'payment-recovery-key' },
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('private, no-store');
    await expect(response.json()).resolves.toEqual({
      paymentStatus: 'unpaid',
      quoteId: quote.id,
      expiresAt: quote.expiresAt,
    });
    expect(settle).not.toHaveBeenCalled();
    expect(github.assertIssueAuthorization).not.toHaveBeenCalled();
    expect(github.currentHead).not.toHaveBeenCalled();
  });

  it('creates and recovers a server-owned payment attempt without browser storage', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const challenge = vi.fn(async () => ({ x402Version: 2, accepts: [{ scheme: 'exact' }] }));
    const base = await serve(dependencies(store, { payments: { challenge } }));

    const created = await fetch(`${base}/v1/account/payment-attempts`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({
        quote_id: quote.id,
        wallet: payment.payer,
        app_build: 'release-f3be9e6',
      }),
    });
    expect(created.status).toBe(201);
    const body = (await created.json()) as {
      attempt: { id: string; idempotencyKey: string; stage: string; retrySafe: boolean };
    };
    expect(body.attempt).toMatchObject({ stage: 'created', retrySafe: true });
    expect(body.attempt.id).toMatch(/^[0-9a-f-]{36}$/i);
    expect(body.attempt.idempotencyKey).toMatch(/^[0-9a-f-]{36}$/i);

    const active = await fetch(`${base}/v1/account/payment-attempts/active`, {
      headers: sessionHeaders,
    });
    expect(active.status).toBe(200);
    await expect(active.json()).resolves.toMatchObject({
      paymentStatus: 'created',
      attempt: { id: body.attempt.id, quoteId: quote.id },
      quote: {
        id: quote.id,
        payment: { x402Version: 2, accepts: [{ scheme: 'exact' }] },
      },
    });
    expect(challenge).toHaveBeenCalledWith(quote);

    for (const stage of ['wallet_opened', 'wallet_signed', 'submitting']) {
      const updated = await fetch(`${base}/v1/account/payment-attempts/${body.attempt.id}/stage`, {
        method: 'PATCH',
        headers: { ...sessionHeaders, 'content-type': 'application/json' },
        body: JSON.stringify({ stage }),
      });
      expect(updated.status).toBe(200);
      await expect(updated.json()).resolves.toMatchObject({
        attempt: { id: body.attempt.id, stage },
      });
    }
  });

  it('keeps an expired signed attempt indeterminate when no job was ever reserved', async () => {
    const store = new MemoryStore();
    const expiredQuote = {
      ...quote,
      id: '22222222-2222-4222-8222-222222222222',
      expiresAt: '2026-01-01T00:00:00.000Z',
    };
    const nextQuote = {
      ...quote,
      id: '33333333-3333-4333-8333-333333333333',
      issueNumber: 8,
    };
    await store.upsertContributor('42', 'maintainer');
    await Promise.all([store.saveQuote(expiredQuote), store.saveQuote(nextQuote)]);
    await Promise.all([
      store.linkQuoteToAccount(expiredQuote.id, '42'),
      store.linkQuoteToAccount(nextQuote.id, '42'),
    ]);
    const attempt = await store.createPaymentAttempt({
      githubId: '42',
      quoteId: expiredQuote.id,
      wallet: payment.payer,
      appBuild: 'release-f3be9e6',
    });
    await store.updatePaymentAttemptStage(attempt.id, '42', 'wallet_opened');
    await store.updatePaymentAttemptStage(attempt.id, '42', 'wallet_signed');
    await store.updatePaymentAttemptStage(attempt.id, '42', 'submitting');
    const base = await serve(dependencies(store));

    const response = await fetch(`${base}/v1/account/payment-attempts/active`, {
      headers: sessionHeaders,
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      paymentStatus: 'indeterminate',
      retrySafe: false,
      attempt: { id: attempt.id, stage: 'indeterminate', retrySafe: false },
    });
    await expect(
      store.createPaymentAttempt({
        githubId: '42',
        quoteId: nextQuote.id,
        wallet: payment.payer,
        appBuild: 'release-f3be9e6',
      }),
    ).rejects.toThrow('resolve the active payment attempt');
  });

  it('binds direct recovery to the reserved job without returning it as a new active attempt', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const attempt = await store.createPaymentAttempt({
      githubId: '42',
      quoteId: quote.id,
      wallet: payment.payer,
      appBuild: 'release-f3be9e6',
    });
    const { job } = await store.createJob(
      quote,
      payment,
      attempt.idempotencyKey,
      undefined,
      attempt.id,
    );
    const base = await serve(dependencies(store));

    const recovered = await fetch(`${base}/v1/account/payment-attempts/${attempt.id}`, {
      headers: sessionHeaders,
    });
    expect(recovered.status).toBe(200);
    await expect(recovered.json()).resolves.toMatchObject({
      paymentStatus: 'job_reserved',
      attempt: { id: attempt.id, jobId: job.id, settlementTransaction: 'settlement' },
      job: { id: job.id },
    });

    const active = await fetch(`${base}/v1/account/payment-attempts/active`, {
      headers: sessionHeaders,
    });
    expect(active.status).toBe(200);
    await expect(active.json()).resolves.toMatchObject({
      paymentStatus: 'job_reserved',
      attempt: { id: attempt.id },
      job: { id: job.id },
    });

    const recreated = await fetch(`${base}/v1/account/payment-attempts`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({
        quote_id: quote.id,
        wallet: payment.payer,
        app_build: 'release-new',
      }),
    });
    expect(recreated.status).toBe(201);
    await expect(recreated.json()).resolves.toMatchObject({
      paymentStatus: 'job_reserved',
      attempt: { id: attempt.id },
      job: { id: job.id },
    });

    await store.transitionJob(job.id, 'settlement_pending', 'delivered');
    const completed = await fetch(`${base}/v1/account/payment-attempts/active`, {
      headers: sessionHeaders,
    });
    expect(completed.status).toBe(200);
    await expect(completed.json()).resolves.toMatchObject({
      attempt: null,
      paymentStatus: 'none',
    });
  });

  it('reuses an unpaid attempt across builds and supersedes it for a different quote', async () => {
    const store = new MemoryStore();
    const nextQuote = { ...quote, id: '22222222-2222-4222-8222-222222222222', issueNumber: 8 };
    await store.upsertContributor('42', 'maintainer');
    await Promise.all([store.saveQuote(quote), store.saveQuote(nextQuote)]);
    await Promise.all([
      store.linkQuoteToAccount(quote.id, '42'),
      store.linkQuoteToAccount(nextQuote.id, '42'),
    ]);
    const attempt = await store.createPaymentAttempt({
      githubId: '42',
      quoteId: quote.id,
      wallet: payment.payer,
      appBuild: 'release-old',
    });

    await expect(
      store.createPaymentAttempt({
        githubId: '42',
        quoteId: quote.id,
        wallet: payment.payer,
        appBuild: 'release-new',
      }),
    ).resolves.toMatchObject({ id: attempt.id, appBuild: 'release-old' });
    await expect(
      store.createPaymentAttempt({
        githubId: '42',
        quoteId: nextQuote.id,
        wallet: payment.payer,
        appBuild: 'release-new',
      }),
    ).resolves.toMatchObject({ quoteId: nextQuote.id, stage: 'created' });
    await expect(store.paymentAttempt(attempt.id, '42')).resolves.toMatchObject({
      stage: 'expired_unpaid',
    });
  });

  it('does not supersede an attempt after the wallet has signed', async () => {
    const store = new MemoryStore();
    const nextQuote = { ...quote, id: '22222222-2222-4222-8222-222222222222', issueNumber: 8 };
    await store.upsertContributor('42', 'maintainer');
    await Promise.all([store.saveQuote(quote), store.saveQuote(nextQuote)]);
    await Promise.all([
      store.linkQuoteToAccount(quote.id, '42'),
      store.linkQuoteToAccount(nextQuote.id, '42'),
    ]);
    const attempt = await store.createPaymentAttempt({
      githubId: '42',
      quoteId: quote.id,
      wallet: payment.payer,
      appBuild: 'release-old',
    });
    await store.updatePaymentAttemptStage(attempt.id, '42', 'wallet_opened');
    await store.updatePaymentAttemptStage(attempt.id, '42', 'wallet_signed');

    await expect(
      store.createPaymentAttempt({
        githubId: '42',
        quoteId: nextQuote.id,
        wallet: payment.payer,
        appBuild: 'release-new',
      }),
    ).rejects.toThrow('resolve the active payment attempt');
  });

  it('binds job creation to the server attempt key and exact paying wallet', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'payment attempt integration test',
      updatedBy: 'test',
    });
    const attempt = await store.createPaymentAttempt({
      githubId: '42',
      quoteId: quote.id,
      wallet: payment.payer,
      appBuild: 'release-f3be9e6',
    });
    const settle = vi.fn(async (_quote, _signature, persist) => {
      await expect(store.paymentAttempt(attempt.id, '42')).resolves.toMatchObject({
        stage: 'submitting',
        retrySafe: false,
      });
      const authorized = {
        ...payment,
        payer: '2'.repeat(32),
        transaction: 'pending',
        signature: 'signed-payment',
      };
      await persist(authorized);
      return { ok: true as const, payment: { ...authorized, transaction: 'settlement' } };
    });
    const base = await serve(
      dependencies(store, {
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        processor: { process: vi.fn(async () => undefined) },
      }),
    );

    const mismatched = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        ...sessionHeaders,
        'content-type': 'application/json',
        'idempotency-key': attempt.idempotencyKey,
        'payment-signature': 'signed-payment',
      },
      body: JSON.stringify({ quote_id: quote.id, payment_attempt_id: attempt.id }),
    });
    expect(mismatched.status).toBe(409);
    await expect(store.jobByQuote(quote.id)).resolves.toBeUndefined();

    settle.mockImplementationOnce(async (_quote, _signature, persist) => {
      const authorized = { ...payment, transaction: 'pending', signature: 'signed-payment' };
      await persist(authorized);
      return { ok: true as const, payment: { ...authorized, transaction: 'settlement' } };
    });
    const accepted = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        ...sessionHeaders,
        'content-type': 'application/json',
        'idempotency-key': attempt.idempotencyKey,
        'payment-signature': 'signed-payment',
      },
      body: JSON.stringify({ quote_id: quote.id, payment_attempt_id: attempt.id }),
    });
    expect(accepted.status).toBe(202);
    await expect(accepted.json()).resolves.toMatchObject({ state: 'paid' });
    await expect(store.jobByQuote(quote.id)).resolves.toMatchObject({
      payment: { payer: payment.payer, transaction: 'settlement' },
      paymentAttemptId: attempt.id,
    });
    await expect(store.paymentAttempt(attempt.id, '42')).resolves.toMatchObject({
      stage: 'job_reserved',
      settlementTransaction: 'settlement',
    });
  });

  it('rejects a malformed payment-status quote id before reading the store', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const quoteForAccount = vi.spyOn(store, 'quoteForAccount');
    const base = await serve(dependencies(store));

    const response = await fetch(`${base}/v1/account/quotes/not-a-uuid/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'malformed-quote-id' },
    });

    expect(response.status).toBe(404);
    await expect(response.json()).resolves.toEqual({ error: 'quote not found' });
    expect(quoteForAccount).not.toHaveBeenCalled();
  });

  it('returns an existing reservation while paid intake is closed', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'reserved-payment-key');
    const base = await serve(
      dependencies(store, { readiness: { check: vi.fn(async () => ({ ready: false })) } }),
    );

    const response = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'reserved-payment-key' },
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      paymentStatus: 'job_reserved',
      quoteId: quote.id,
      job: { id: job.id, state: 'settlement_pending' },
    });
  });

  it('rejects payment recovery outside the quote account or with a reused key', async () => {
    const store = new MemoryStore();
    const otherQuote = { ...quote, id: '22222222-2222-4222-8222-222222222222', issueNumber: 8 };
    await store.upsertContributor('42', 'maintainer');
    await Promise.all([store.saveQuote(quote), store.saveQuote(otherQuote)]);
    await Promise.all([
      store.linkQuoteToAccount(quote.id, '42'),
      store.linkQuoteToAccount(otherQuote.id, '42'),
    ]);
    await store.createJob(otherQuote, payment, 'other-quote-key');
    const base = await serve(dependencies(store));

    const conflict = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'other-quote-key' },
    });
    expect(conflict.status).toBe(409);
    await expect(conflict.json()).resolves.toEqual({ error: 'idempotency key already used' });

    const anonymous = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { 'idempotency-key': 'payment-recovery-key' },
    });
    expect(anonymous.status).toBe(401);

    const missingKey = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: sessionHeaders,
    });
    expect(missingKey.status).toBe(400);
  });

  it('keeps payment-status reads out of the commercial admission lane', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const gate = new SerialGate();
    const run = vi.spyOn(gate, 'run');
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const active = gate.run(async () => blocked);
    const base = await serve(dependencies(store, { paymentAdmission: gate }));
    const response = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'in-flight-payment-key' },
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({ paymentStatus: 'unpaid' });
    expect(run).toHaveBeenCalledTimes(1);
    release();
    await active;
  });

  it('bounds the serial gate queue without letting a timed-out waiter break serialization', async () => {
    const gate = new SerialGate(undefined, { maxQueued: 1, acquireTimeoutMs: 20 });
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const active = gate.run(async () => blocked);
    const timedOutOperation = vi.fn(async () => 'timed-out');
    const timedOut = gate.run(timedOutOperation);

    await expect(gate.run(async () => 'overflow')).rejects.toThrow(
      'payment processing is temporarily busy',
    );
    await expect(timedOut).rejects.toThrow('payment processing is temporarily busy');
    expect(timedOutOperation).not.toHaveBeenCalled();

    const nextOperation = vi.fn(async () => 'next');
    const next = gate.run(nextOperation);
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(nextOperation).not.toHaveBeenCalled();
    release();

    await expect(active).resolves.toBeUndefined();
    await expect(next).resolves.toBe('next');
    expect(nextOperation).toHaveBeenCalledOnce();
  });

  it('rate limits payment-status reads without entering the settlement gate', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const gate = new SerialGate();
    const run = vi.spyOn(gate, 'run');
    const base = await serve(dependencies(store, { paymentAdmission: gate }));

    for (let index = 0; index < 12; index += 1) {
      const response = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
        headers: { ...sessionHeaders, 'idempotency-key': `payment-status-${index}` },
      });
      expect(response.status).toBe(200);
    }
    const limited = await fetch(`${base}/v1/account/quotes/${quote.id}/payment-status`, {
      headers: { ...sessionHeaders, 'idempotency-key': 'payment-status-limited' },
    });
    expect(limited.status).toBe(429);
    expect(run).not.toHaveBeenCalled();
  });

  it('reports a confirming payment once, then replaces it with one finalized payment', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const pendingPayment = { ...payment, transaction: 'pending' };
    const { job } = await store.createJob(quote, pendingPayment, 'confirming-payment');
    const base = await serve(dependencies(store));

    const confirming = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });
    const confirmingBody = (await confirming.json()) as {
      totals: { confirmingAtomic: string; paidAtomic: string };
      transactions: Array<Record<string, unknown>>;
    };
    expect(confirmingBody.totals).toMatchObject({
      confirmingAtomic: '2000000',
      paidAtomic: '0',
    });
    expect(confirmingBody.transactions).toEqual([
      expect.objectContaining({
        id: `payment:${job.id}`,
        jobId: job.id,
        type: 'payment',
        status: 'pending',
        transaction: null,
      }),
    ]);

    await store.transitionJob(job.id, 'settlement_pending', 'paid', { payment });
    const finalized = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });
    const finalizedBody = (await finalized.json()) as {
      totals: { confirmingAtomic: string; paidAtomic: string };
      transactions: Array<Record<string, unknown>>;
    };
    expect(finalizedBody.totals).toMatchObject({
      confirmingAtomic: '0',
      paidAtomic: '2000000',
    });
    expect(finalizedBody.transactions).toEqual([
      expect.objectContaining({
        id: `payment:${job.id}`,
        jobId: job.id,
        type: 'payment',
        status: 'finalized',
        transaction: 'settlement',
      }),
    ]);
  });

  it.each(['failed', 'rejected', 'refund_pending'] as const)(
    'reports a pending refund for a %s job without presenting a finalized transaction',
    async (state) => {
      const store = new MemoryStore();
      await store.upsertContributor('42', 'maintainer');
      await store.saveQuote(quote);
      await store.linkQuoteToAccount(quote.id, '42');
      const { job } = await store.createJob(quote, payment, 'refund-pending-job');
      await store.transitionJob(job.id, 'settlement_pending', state);
      const base = await serve(dependencies(store));

      const response = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });
      const body = (await response.json()) as {
        transactions: Array<Record<string, unknown>>;
      };

      expect(body.transactions).toEqual(
        expect.arrayContaining([
          expect.objectContaining({
            jobId: job.id,
            type: 'refund',
            status: 'pending',
            transaction: null,
          }),
          expect.objectContaining({
            jobId: job.id,
            type: 'payment',
            status: 'finalized',
            transaction: 'settlement',
          }),
        ]),
      );
    },
  );

  it('marks billing totals as a bounded window when account history is truncated', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    vi.spyOn(store, 'jobsForAccount').mockResolvedValueOnce({
      jobs: [],
      limit: 1_000,
      truncated: true,
      obligationCount: 3,
    });
    const base = await serve(dependencies(store));

    const response = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });

    await expect(response.json()).resolves.toMatchObject({
      limit: 1_000,
      truncated: true,
      obligationCount: 3,
      totalsScope: 'latest_terminal_jobs_and_all_obligations',
    });
  });

  it('clears the session cookie on logout', async () => {
    const base = await serve(dependencies(new MemoryStore()));
    const response = await fetch(`${base}/v1/auth/logout`, {
      method: 'POST',
      headers: sessionHeaders,
    });

    expect(response.status).toBe(200);
    expect(response.headers.get('set-cookie')).toContain('mizuki_session=;');
    expect(response.headers.get('set-cookie')).toContain('Max-Age=0');
    expect(response.headers.get('clear-site-data')).toBe('"cache", "cookies", "storage"');
    expect(response.headers.get('cache-control')).toBe('private, no-store');
  });

  it('returns verified repository, issue, and preflight contracts', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const issue = {
      number: 7,
      title: 'Fix the documentation typo',
      url: 'https://github.com/example/project/issues/7',
      labels: ['mizuki:authorized'],
      authorized: true,
      scopeEligible: true,
      eligibility: true,
      class: 'micro' as const,
      priceAtomic: '2000000',
      maxFiles: 3,
      validationCommands: ['npm test'],
    };
    const repository = {
      owner: 'example',
      repo: 'project',
      repository: 'example/project',
      defaultBranch: 'main',
      installationId: 10,
      permission: 'maintain' as const,
      rootFiles: ['package-lock.json'],
    };
    const github = {
      repositoryMetadataForMaintainer: vi.fn(async () => repository),
      issuesForMaintainer: vi.fn(async () => ({ repository, issues: [issue] })),
      preflightIssue: vi.fn(async () => ({
        owner: 'example',
        repo: 'project',
        repository: 'example/project',
        defaultBranch: 'main',
        core: { status: 'ready' as const, installationId: 10 },
        maintainer: { verified: true, permission: 'maintain' as const },
        issue,
        blockers: [],
      })),
    };
    const policy = {
      assertRepositoryReady: vi.fn(async () => ({ verifierAppId: '20', installationId: 30 })),
    };
    const base = await serve(dependencies(store, { github, policy }));

    const connected = await fetch(`${base}/v1/account/repositories`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ repository: 'example/project' }),
    });
    expect(connected.status).toBe(201);
    await expect(connected.json()).resolves.toMatchObject({
      repository: {
        repository: 'example/project',
        core: { status: 'ready' },
        policy: { status: 'ready' },
      },
    });
    const linked = await store.repositoriesForAccount('42', 25);
    expect(linked.repositories).toHaveLength(1);

    const repositories = await fetch(`${base}/v1/account/repositories`, {
      headers: sessionHeaders,
    });
    await expect(repositories.json()).resolves.toMatchObject({
      repositories: [
        {
          repository: 'example/project',
          permission: 'maintain',
          core: { status: 'ready' },
          policy: { status: 'ready' },
          readyForWork: true,
        },
      ],
      limit: 25,
      truncated: false,
    });

    const issues = await fetch(`${base}/v1/repositories/example/project/issues`, {
      headers: sessionHeaders,
    });
    await expect(issues.json()).resolves.toEqual({ issues: [issue] });
    await expect(store.repositoriesForAccount('42', 25)).resolves.toEqual(linked);

    const preflight = await fetch(`${base}/v1/preflights`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issue.url }),
    });
    await expect(preflight.json()).resolves.toMatchObject({
      repository: { repository: 'example/project' },
      checks: {
        core: { status: 'ready' },
        policy: { status: 'ready' },
        maintainer: { status: 'ready' },
        authorization: { status: 'ready' },
        eligibility: { status: 'ready' },
      },
      class: 'micro',
      priceAtomic: '2000000',
      readyForWork: true,
      blockers: [],
    });
  });

  it('rejects a twenty-sixth repository with a conflict response', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await Promise.all(
      Array.from({ length: 25 }, (_, index) =>
        store.linkAccountRepository('42', 'example', `project-${index}`),
      ),
    );
    const base = await serve(
      dependencies(store, {
        github: {
          repositoryMetadataForMaintainer: vi.fn(async () => ({
            ...repositoryMetadata,
            repo: 'project-25',
            repository: 'example/project-25',
          })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/account/repositories`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ repository: 'example/project-25' }),
    });

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: 'account repository limit of 25 reached',
    });
  });

  it('requires an explicit repository link for authenticated issue, preflight, and quote routes', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'open intake for repository boundary test',
      updatedBy: 'operator',
    });
    const issue = vi.fn(async () => githubIssue);
    const issuesForMaintainer = vi.fn();
    const preflightIssue = vi.fn();
    const repositoryMetadataForMaintainer = vi.fn();
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: { issue, issuesForMaintainer, preflightIssue, repositoryMetadataForMaintainer },
        payments: { challenge },
      }),
    );

    const issues = await fetch(`${base}/v1/repositories/example/project/issues`, {
      headers: sessionHeaders,
    });
    expect(issues.status).toBe(403);

    const preflight = await fetch(`${base}/v1/preflights`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(preflight.status).toBe(403);

    const quoteResponse = await fetch(`${base}/v1/account/quotes`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(quoteResponse.status).toBe(403);
    expect(issue).not.toHaveBeenCalled();
    expect(issuesForMaintainer).not.toHaveBeenCalled();
    expect(preflightIssue).not.toHaveBeenCalled();
    expect(repositoryMetadataForMaintainer).not.toHaveBeenCalled();
    expect(challenge).not.toHaveBeenCalled();
    await expect(store.repositoriesForAccount('42', 25)).resolves.toMatchObject({
      repositories: [],
    });

    const staleSession = await fetch(`${base}/v1/account/quotes`, {
      method: 'POST',
      headers: { cookie: 'mizuki_session=expired', 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(staleSession.status).toBe(401);
    expect(issue).not.toHaveBeenCalled();
  });

  it('uses a bounded account bounty query and exposes its page metadata', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const query = vi.spyOn(store, 'bountiesForAccount').mockResolvedValueOnce({
      bounties: [],
      limit: 100,
      truncated: true,
    });
    const base = await serve(dependencies(store));

    const response = await fetch(`${base}/v1/account/bounties`, { headers: sessionHeaders });

    await expect(response.json()).resolves.toEqual({
      bounties: [],
      limit: 100,
      truncated: true,
    });
    expect(query).toHaveBeenCalledWith('42', 100);
  });

  it('rate limits GitHub-backed account reads across source rotation', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    const secret = 'p'.repeat(32);
    const deps = dependencies(store, {
      github: {
        repositoryMetadataForMaintainer: vi.fn(async () => repositoryMetadata),
      },
      policy: {
        assertRepositoryReady: vi.fn(async () => ({ verifierAppId: '20', installationId: 30 })),
      },
    });
    deps.config.webProxySecret = secret;
    const base = await serve(deps);
    const headers = (index: number) => ({
      ...sessionHeaders,
      'x-mizuki-client-ip': `198.51.100.${index + 1}`,
      'x-mizuki-proxy-secret': secret,
    });

    for (let index = 0; index < 12; index += 1) {
      const response = await fetch(`${base}/v1/account/repositories`, {
        headers: headers(index),
      });
      expect(response.status).toBe(200);
    }
    const limited = await fetch(`${base}/v1/account/repositories`, {
      headers: headers(12),
    });
    expect(limited.status).toBe(429);
  });

  it('links a signed-in quote to its verified maintainer account', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'open intake for account quote test',
      updatedBy: 'operator',
    });
    const repository = {
      owner: 'example',
      repo: 'project',
      repository: 'example/project',
      defaultBranch: 'main',
      installationId: 10,
      permission: 'maintain' as const,
      rootFiles: ['package-lock.json'],
    };
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => githubIssue),
          repositoryMetadataForMaintainer: vi.fn(async () => repository),
        },
        payments: { challenge: vi.fn(async () => ({ scheme: 'mock' })) },
      }),
    );

    const response = await fetch(`${base}/v1/account/quotes`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(response.status).toBe(201);
    const created = (await response.json()) as Quote;
    await store.createJob(created, payment, 'linked-job');
    await expect(store.jobsForAccount('42', 100)).resolves.toMatchObject({
      jobs: [expect.any(Object)],
      truncated: false,
      obligationCount: 1,
    });
  });

  it('does not issue a payable quote when verified account linking is not durable', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'open intake for durable account link test',
      updatedBy: 'operator',
    });
    vi.spyOn(store, 'linkQuoteToAccount').mockRejectedValueOnce(new Error('database write failed'));
    const challenge = vi.fn(async () => ({ scheme: 'mock' }));
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => githubIssue),
          repositoryMetadataForMaintainer: vi.fn(async () => repositoryMetadata),
        },
        payments: { challenge },
      }),
    );

    const response = await fetch(`${base}/v1/account/quotes`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });

    expect(response.status).toBe(500);
    expect(challenge).not.toHaveBeenCalled();
  });

  it('keeps the public quote route anonymous even when the browser has a session', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'contributor');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'open intake for anonymous quote compatibility',
      updatedBy: 'operator',
    });
    const base = await serve(
      dependencies(store, {
        github: { issue: vi.fn(async () => githubIssue) },
        payments: { challenge: vi.fn(async () => ({ scheme: 'mock' })) },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    expect(response.status).toBe(201);
    const created = (await response.json()) as Quote;
    await store.createJob(created, payment, 'anonymous-contributor-job');
    await expect(store.jobsForAccount('42', 100)).resolves.toEqual({
      jobs: [],
      limit: 100,
      truncated: false,
      obligationCount: 0,
    });
  });

  it.each([
    [
      new PolicyRequestError('github_app_not_installed', 404, 'not installed'),
      'action_required',
      'Install the read-only policy verifier on this repository.',
    ],
    [
      new Error('signer unavailable'),
      'unavailable',
      'The read-only policy verifier is temporarily unavailable.',
    ],
  ])('reports policy readiness without misdiagnosing outages', async (failure, status, reason) => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    const base = await serve(
      dependencies(store, {
        github: {
          preflightIssue: vi.fn(async () => ({
            owner: 'example',
            repo: 'project',
            repository: 'example/project',
            defaultBranch: 'main',
            core: { status: 'ready', installationId: 10 },
            maintainer: { verified: true, permission: 'maintain' },
            issue: {
              number: 7,
              title: 'Fix docs typo',
              url: issueUrl,
              labels: ['mizuki:authorized'],
              authorized: true,
              scopeEligible: true,
              eligibility: true,
              class: 'micro',
              priceAtomic: '2000000',
              maxFiles: 3,
              validationCommands: ['npm test'],
            },
            blockers: [],
          })),
        },
        policy: {
          assertRepositoryReady: vi.fn(async () => {
            throw failure;
          }),
        },
      }),
    );

    const response = await fetch(`${base}/v1/preflights`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });
    await expect(response.json()).resolves.toMatchObject({
      checks: { policy: { status, reason } },
      blockers: [reason],
      readyForWork: false,
    });
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
  return {
    config: {
      paymentMode: 'mock',
      webOrigin: 'https://mizuki.example',
      trustedProxyHops: 0,
      rateLimitMaxSources: 100,
      sseMaxConnections: 10,
      sseMaxConnectionsPerSource: 2,
      githubAuthorizationLabel: 'mizuki:authorized',
    },
    store,
    github: {},
    payments: {},
    processor: {},
    auth: {
      session: vi.fn((value: string | undefined) =>
        value === 'session'
          ? { githubId: '42', githubLogin: 'maintainer', exp: Date.now() + 60_000 }
          : undefined,
      ),
      csrfToken: vi.fn((value: string | undefined) =>
        value === 'session' ? 'c'.repeat(43) : undefined,
      ),
      verifyCsrfToken: vi.fn(
        (value: string | undefined, token: string | undefined) =>
          value === 'session' && token === 'c'.repeat(43),
      ),
    },
    webhooks: {},
    bounties: {},
    policy: {},
    paymentAdmission: new SerialGate(),
    readiness: { check: vi.fn(async () => ({ ready: true })) },
    ...overrides,
  } as unknown as AppDependencies;
}

const sessionHeaders = {
  cookie: 'mizuki_session=session',
  origin: 'https://mizuki.example',
  'x-mizuki-csrf-token': 'c'.repeat(43),
};
const issueUrl = 'https://github.com/example/project/issues/7';
const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl,
  owner: 'example',
  repo: 'project',
  issueNumber: 7,
  issueTitle: 'Fix docs typo',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: ['npm test'],
  expiresAt: '2099-01-01T00:00:00.000Z',
};
const payment: Payment = {
  payer: '1'.repeat(32),
  transaction: 'settlement',
  amountAtomic: '2000000',
};

const githubIssue: GithubIssue = {
  owner: 'example',
  repo: 'project',
  number: 7,
  title: 'Fix docs typo',
  body: 'Correct docs/guide.md.',
  labels: ['mizuki:authorized'],
  defaultBranch: 'main',
  baseSha: 'a'.repeat(40),
  rootFiles: ['package-lock.json'],
  installationId: 10,
  authorizationReceipt: {
    label: 'mizuki:authorized',
    actorId: '42',
    actorLogin: 'maintainer',
    permission: 'maintain',
    authorizedAt: '2026-08-25T00:00:00.000Z',
    verifiedAt: '2026-08-25T00:00:01.000Z',
    evidenceHash: 'b'.repeat(64),
  },
};

const repositoryMetadata = {
  owner: 'example',
  repo: 'project',
  repository: 'example/project',
  defaultBranch: 'main',
  installationId: 10,
  permission: 'maintain' as const,
};
