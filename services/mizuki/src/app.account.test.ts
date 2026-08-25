import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApp, SerialGate, type AppDependencies } from './app.js';
import { GithubAccessError, GithubReadinessError } from './github.js';
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
  it('returns 503 when authenticated repository access is unavailable', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.linkAccountRepository('42', 'example', 'project');
    vi.spyOn(console, 'error').mockImplementation(() => {});
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
      }),
    );

    const response = await fetch(`${base}/v1/account/repositories`, {
      headers: sessionHeaders,
    });

    expect(response.status).toBe(503);
    expect(response.headers.get('x-request-id')).toBeTruthy();
    await expect(response.json()).resolves.toEqual({
      error: 'GitHub repository access is temporarily unavailable. Please try again shortly.',
    });
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
    });

    const billing = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });
    await expect(billing.json()).resolves.toMatchObject({
      mode: 'mock',
      asset: 'USDC',
      limit: 1000,
      truncated: false,
      totalsScope: 'account_lifetime',
      totals: {
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

  it('reports a pending refund without presenting a finalized transaction', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'refund-pending-job');
    await store.transitionJob(job.id, 'settlement_pending', 'refund_pending');
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
  });

  it('marks billing totals as a bounded window when account history is truncated', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    vi.spyOn(store, 'jobsForAccount').mockResolvedValueOnce({
      jobs: [],
      limit: 1_000,
      truncated: true,
    });
    const base = await serve(dependencies(store));

    const response = await fetch(`${base}/v1/account/billing`, { headers: sessionHeaders });

    await expect(response.json()).resolves.toMatchObject({
      limit: 1_000,
      truncated: true,
      totalsScope: 'latest_jobs',
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

    const response = await fetch(`${base}/v1/quotes`, {
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
    });
  });

  it('does not issue a payable quote when verified account linking is not durable', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      reason: 'open intake for durable account link test',
      updatedBy: 'operator',
    });
    vi.spyOn(store, 'linkAccountRepository').mockRejectedValueOnce(
      new Error('database write failed'),
    );
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

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { ...sessionHeaders, 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: issueUrl }),
    });

    expect(response.status).toBe(500);
    expect(challenge).not.toHaveBeenCalled();
  });

  it('keeps a signed-in contributor quote anonymous when it does not maintain the repository', async () => {
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
        github: {
          issue: vi.fn(async () => githubIssue),
          repositoryMetadataForMaintainer: vi.fn(async () => {
            throw new GithubAccessError('repository maintainer access is required');
          }),
        },
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
    },
    webhooks: {},
    bounties: {},
    policy: {},
    paymentAdmission: new SerialGate(),
    readiness: { check: vi.fn(async () => ({ ready: true })) },
    ...overrides,
  } as unknown as AppDependencies;
}

const sessionHeaders = { cookie: 'mizuki_session=session' };
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
