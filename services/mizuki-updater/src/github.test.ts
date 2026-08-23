import { generateKeyPairSync } from 'node:crypto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { GitHubAppGateway } from './github.js';
import { proposalFixture } from './test-utils.js';

describe('GitHub App gateway', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('accepts successful checks from their exact trusted workflow runs', async () => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal('fetch', vi.fn(admissionFetch(manifest)));

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toEqual({
      status: 'passed',
      checks: { test: 'success', security: 'success' },
    });
  });

  it('treats an absent required check as pending', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const fixture = admissionFixture(manifest);
    const admitted = admissionFetch(manifest, { checkRuns: [fixture.checkRuns[0]!] });
    let workflowReads = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.includes('/actions/workflows/') || url.includes('/contents/')) workflowReads += 1;
        return admitted(input);
      }),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toEqual({
      status: 'pending',
      checks: { test: 'success', security: 'missing' },
    });
    expect(workflowReads).toBe(0);
  });

  it('fails a spoofed same-name check even when the trusted check passed', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const fixture = admissionFixture(manifest);
    vi.stubGlobal(
      'fetch',
      vi.fn(
        admissionFetch(manifest, {
          checkRuns: [
            ...fixture.checkRuns,
            { ...fixture.checkRuns[0]!, id: 99, app: { id: 999 }, check_suite: { id: 999 } },
          ],
        }),
      ),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toEqual({
      status: 'failed',
      checks: { test: 'untrusted-producer', security: 'success' },
    });
  });

  it.each([
    ['workflow ID', (run: Record<string, unknown>) => ({ ...run, workflow_id: 999 })],
    [
      'workflow path',
      (run: Record<string, unknown>) => ({ ...run, path: '.github/workflows/other.yml' }),
    ],
    ['event', (run: Record<string, unknown>) => ({ ...run, event: 'push' })],
    ['head ref', (run: Record<string, unknown>) => ({ ...run, head_branch: 'other/head' })],
    [
      'repository',
      (run: Record<string, unknown>) => ({
        ...run,
        repository: { id: 77, full_name: 'other/repository' },
      }),
    ],
    [
      'head repository',
      (run: Record<string, unknown>) => ({
        ...run,
        head_repository: { id: 88, full_name: 'other/repository' },
      }),
    ],
    ['candidate commit', (run: Record<string, unknown>) => ({ ...run, head_sha: 'e'.repeat(40) })],
    ['base ref', (run: Record<string, unknown>) => mutatePull(run, { base: { ref: 'release' } })],
    [
      'base commit',
      (run: Record<string, unknown>) => mutatePull(run, { base: { sha: 'e'.repeat(40) } }),
    ],
  ])('fails a same-name check from the wrong %s', async (_case, mutate) => {
    const manifest = proposalFixture().proposal.manifest;
    const fixture = admissionFixture(manifest);
    vi.stubGlobal(
      'fetch',
      vi.fn(
        admissionFetch(manifest, {
          workflowRuns: [mutate(fixture.workflowRuns[0]!), fixture.workflowRuns[1]!],
        }),
      ),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toEqual({
      status: 'failed',
      checks: { test: 'untrusted-workflow', security: 'success' },
    });
  });

  it('rejects checks when the candidate modified the pinned workflow definition', async () => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal(
      'fetch',
      vi.fn(admissionFetch(manifest, { candidateDefinitionSha: 'e'.repeat(40) })),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toEqual({
      status: 'failed',
      checks: {
        test: 'workflow-definition-changed',
        security: 'workflow-definition-changed',
      },
    });
  });

  it('rejects check admission when the pull request no longer targets the signed base', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const pull = openPull(manifest);
    vi.stubGlobal(
      'fetch',
      vi.fn(
        admissionFetch(manifest, {
          pull: { ...pull, base: { ...pull.base, sha: 'e'.repeat(40) } },
        }),
      ),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).rejects.toMatchObject({
      code: 'pull_request_changed',
    });
  });

  it('creates a PR only after confirming the branch SHA and embeds the manifest receipt', async () => {
    const proposal = proposalFixture().proposal;
    const manifest = proposal.manifest;
    let createBody: Record<string, unknown> | null = null;
    let tokenBody: Record<string, unknown> | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) {
          tokenBody = JSON.parse(String(init?.body));
          return Response.json(updaterToken());
        }
        if (url.includes('/git/ref/heads/')) {
          return Response.json(refBody(url, manifest));
        }
        if (url.includes('/pulls?')) return Response.json([]);
        if (url.endsWith('/pulls') && init?.method === 'POST') {
          createBody = JSON.parse(String(init.body));
          return Response.json({
            number: 42,
            html_url: 'https://github.com/mizuki-labs/mizuki/pull/42',
            state: 'open',
            head: { sha: manifest.candidateSha },
            base: {
              ref: manifest.repository.baseBranch,
              sha: manifest.repository.baseSha,
            },
          });
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(
      createGateway().syncPullRequest(manifest, proposal.manifestSha256),
    ).resolves.toEqual({
      number: 42,
      url: 'https://github.com/mizuki-labs/mizuki/pull/42',
    });
    expect(createBody).toMatchObject({
      head: manifest.repository.headBranch,
      base: manifest.repository.baseBranch,
    });
    expect(String(createBody?.body)).toContain(proposal.manifestSha256);
    expect(tokenBody).toEqual({
      repositories: ['mizuki'],
      permissions: updaterPermissions(),
    });
  });

  it('returns the existing merge receipt after a crash instead of merging twice', async () => {
    const manifest = proposalFixture().proposal.manifest;
    let mergeRequests = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) {
          return Response.json(updaterToken());
        }
        if (url.includes('/git/ref/heads/')) throw new Error('merged recovery read a branch ref');
        if (url.endsWith('/pulls/42') && init?.method !== 'PUT') {
          return Response.json({
            number: 42,
            html_url: 'https://github.com/mizuki-labs/mizuki/pull/42',
            state: 'closed',
            merged_at: '2026-08-22T12:10:00Z',
            merge_commit_sha: 'b'.repeat(40),
            head: { sha: manifest.candidateSha },
            base: {
              ref: manifest.repository.baseBranch,
              sha: manifest.repository.baseSha,
            },
          });
        }
        if (init?.method === 'PUT') mergeRequests += 1;
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().merge(manifest, 42)).resolves.toEqual({
      mergeSha: 'b'.repeat(40),
    });
    expect(mergeRequests).toBe(0);
  });

  it('validates the App identity and exact permission contract during readiness', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/app')) {
          return Response.json({ id: 123, permissions: updaterPermissions() });
        }
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) return Response.json(updaterToken());
        if (url.endsWith('/actions/workflows/101')) {
          return Response.json({ id: 101, path: '.github/workflows/test.yml', state: 'active' });
        }
        if (url.endsWith('/actions/workflows/102')) {
          return Response.json({
            id: 102,
            path: '.github/workflows/security.yml',
            state: 'active',
          });
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().readiness()).resolves.toBeUndefined();

    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/app')) {
          return Response.json({ id: 123, permissions: updaterPermissions() });
        }
        if (url.endsWith('/installation')) {
          return Response.json(updaterInstallation({ repository_selection: 'all' }));
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().readiness()).rejects.toThrow();

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ id: 456, permissions: updaterPermissions() })),
    );
    await expect(createGateway().readiness()).rejects.toThrow('different App');

    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        Response.json({
          id: 123,
          permissions: { ...updaterPermissions(), workflows: 'write' },
        }),
      ),
    );
    await expect(createGateway().readiness()).rejects.toThrow('permissions do not match');
  });

  it('rejects workflow identity drift during readiness', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/app')) {
          return Response.json({ id: 123, permissions: updaterPermissions() });
        }
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) return Response.json(updaterToken());
        if (url.endsWith('/actions/workflows/101')) {
          return Response.json({ id: 101, path: '.github/workflows/spoof.yml', state: 'active' });
        }
        if (url.endsWith('/actions/workflows/102')) {
          return Response.json({
            id: 102,
            path: '.github/workflows/security.yml',
            state: 'active',
          });
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );

    await expect(createGateway().readiness()).rejects.toThrow('workflow identity');
  });

  it.each([
    ['all-repository selection', { repository_selection: 'all' }],
    ['an extra permission', { permissions: { ...updaterPermissions(), workflows: 'read' } }],
    ['a missing permission', { permissions: { ...updaterPermissions(), checks: undefined } }],
    ['another repository', { repositories: [{ name: 'other', full_name: 'mizuki-labs/other' }] }],
  ])('rejects a token response with %s', async (_case, overrides) => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) {
          const body = updaterToken(overrides);
          if (body.permissions && typeof body.permissions === 'object') {
            body.permissions = Object.fromEntries(
              Object.entries(body.permissions).filter(([, value]) => value !== undefined),
            );
          }
          return Response.json(body);
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().requiredChecks(manifest, 42)).rejects.toThrow();
  });

  it.each([
    ['another App', { app_id: 456 }],
    ['all repositories', { repository_selection: 'all' }],
    [
      'an extra installation permission',
      { permissions: { ...updaterPermissions(), workflows: 'read' } },
    ],
  ])('rejects an installation bound to %s', async (_case, overrides) => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/installation')) {
          return Response.json(updaterInstallation(overrides));
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().requiredChecks(manifest, 42)).rejects.toThrow();
  });

  it('fails checks before reading results when the signed base revision advanced', async () => {
    const manifest = proposalFixture().proposal.manifest;
    let checkReads = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) return Response.json(updaterToken());
        if (url.endsWith(`/git/ref/heads/${manifest.repository.baseBranch}`)) {
          return Response.json({ object: { sha: 'e'.repeat(40) } });
        }
        if (url.includes('/git/ref/heads/')) return Response.json(refBody(url, manifest));
        if (url.includes('/check-runs') || url.includes('/status?')) checkReads += 1;
        throw new Error(`Unexpected request: ${url}`);
      }),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).rejects.toMatchObject({
      code: 'base_branch_changed',
    });
    expect(checkReads).toBe(0);
  });

  it('fails before merge when the protected base advances concurrently', async () => {
    const manifest = proposalFixture().proposal.manifest;
    let mergeRequests = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) return Response.json(updaterToken());
        if (url.endsWith('/pulls/42') && init?.method !== 'PUT') {
          return Response.json(openPull(manifest));
        }
        if (url.endsWith(`/git/ref/heads/${manifest.repository.baseBranch}`)) {
          return Response.json({ object: { sha: 'e'.repeat(40) } });
        }
        if (url.includes('/git/ref/heads/')) return Response.json(refBody(url, manifest));
        if (init?.method === 'PUT') mergeRequests += 1;
        throw new Error(`Unexpected request: ${url}`);
      }),
    );

    await expect(createGateway().merge(manifest, 42)).rejects.toMatchObject({
      code: 'base_branch_changed',
    });
    expect(mergeRequests).toBe(0);
  });

  it('invalidates a 401 token and retries once with a fresh repository token', async () => {
    const manifest = proposalFixture().proposal.manifest;
    const admitted = admissionFetch(manifest);
    let mints = 0;
    let rejected = false;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json(updaterInstallation());
        if (url.endsWith('/access_tokens')) {
          mints += 1;
          return Response.json(updaterToken({ token: `updater-repository-token-${mints}` }));
        }
        const authorization = new Headers(init?.headers).get('authorization');
        if (!rejected && authorization?.endsWith('-1')) {
          rejected = true;
          return new Response('expired', { status: 401 });
        }
        return admitted(input);
      }),
    );

    await expect(createGateway().requiredChecks(manifest, 42)).resolves.toMatchObject({
      status: 'passed',
    });
    expect(mints).toBe(2);
  });
});

function createGateway(): GitHubAppGateway {
  const pair = generateKeyPairSync('rsa', { modulusLength: 2048 });
  return new GitHubAppGateway({
    apiUrl: 'https://api.github.test',
    appId: 123,
    privateKey: pair.privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    repositories: new Set(['mizuki-labs/mizuki']),
    timeoutMs: 1_000,
    mergeMethod: 'squash',
    checkProducers: new Map([
      ['test', workflowPolicy(101, '.github/workflows/test.yml')],
      ['security', workflowPolicy(102, '.github/workflows/security.yml')],
    ]),
  });
}

function workflowPolicy(workflowId: number, workflowPath: string) {
  return {
    checkRunAppId: 15_368,
    workflowId,
    workflowPath,
    event: 'pull_request' as const,
    headBranch: 'manifest' as const,
    headSha: 'candidate' as const,
    baseBranch: 'manifest' as const,
    baseSha: 'signed' as const,
    definitionRef: 'base' as const,
  };
}

function updaterPermissions() {
  return {
    actions: 'read',
    checks: 'read',
    contents: 'write',
    metadata: 'read',
    pull_requests: 'write',
  } as const;
}

function updaterInstallation(overrides: Record<string, unknown> = {}) {
  return {
    id: 9,
    app_id: 123,
    repository_selection: 'selected',
    permissions: updaterPermissions(),
    suspended_at: null,
    ...overrides,
  };
}

function updaterToken(overrides: Record<string, unknown> = {}) {
  return {
    token: 'updater-installation-token-for-one-repository',
    expires_at: new Date(Date.now() + 60 * 60_000).toISOString(),
    permissions: updaterPermissions(),
    repository_selection: 'selected',
    repositories: [{ name: 'mizuki', full_name: 'mizuki-labs/mizuki' }],
    ...overrides,
  };
}

function refBody(
  url: string,
  manifest: ReturnType<typeof proposalFixture>['proposal']['manifest'],
) {
  return {
    object: {
      sha: url.endsWith(`/${manifest.repository.baseBranch}`)
        ? manifest.repository.baseSha
        : manifest.candidateSha,
    },
  };
}

function openPull(manifest: ReturnType<typeof proposalFixture>['proposal']['manifest']) {
  const repository = `${manifest.repository.owner}/${manifest.repository.name}`;
  return {
    number: 42,
    html_url: 'https://github.com/mizuki-labs/mizuki/pull/42',
    state: 'open',
    merged_at: null,
    merge_commit_sha: null,
    head: {
      ref: manifest.repository.headBranch,
      sha: manifest.candidateSha,
      repo: { id: 77, full_name: repository },
    },
    base: {
      ref: manifest.repository.baseBranch,
      sha: manifest.repository.baseSha,
      repo: { id: 77, full_name: repository },
    },
  };
}

function admissionFixture(manifest: ReturnType<typeof proposalFixture>['proposal']['manifest']) {
  const test = workflowPolicy(101, '.github/workflows/test.yml');
  const security = workflowPolicy(102, '.github/workflows/security.yml');
  return {
    checkRuns: [
      checkRun(manifest, { id: 1, name: 'test', suiteId: 501 }),
      checkRun(manifest, { id: 2, name: 'security', suiteId: 502 }),
    ],
    workflowRuns: [workflowRun(manifest, test, 501), workflowRun(manifest, security, 502)],
  };
}

function checkRun(
  manifest: ReturnType<typeof proposalFixture>['proposal']['manifest'],
  input: { id: number; name: string; suiteId: number },
) {
  return {
    id: input.id,
    name: input.name,
    head_sha: manifest.candidateSha,
    status: 'completed',
    conclusion: 'success',
    app: { id: 15_368 },
    check_suite: { id: input.suiteId },
  };
}

function workflowRun(
  manifest: ReturnType<typeof proposalFixture>['proposal']['manifest'],
  policy: ReturnType<typeof workflowPolicy>,
  checkSuiteId: number,
) {
  const repository = `${manifest.repository.owner}/${manifest.repository.name}`;
  return {
    id: policy.workflowId + 1_000,
    check_suite_id: checkSuiteId,
    head_branch: manifest.repository.headBranch,
    head_sha: manifest.candidateSha,
    path: policy.workflowPath,
    event: policy.event,
    workflow_id: policy.workflowId,
    repository: { id: 77, full_name: repository },
    head_repository: { id: 77, full_name: repository },
    pull_requests: [
      {
        number: 42,
        head: {
          ref: manifest.repository.headBranch,
          sha: manifest.candidateSha,
          repo: { id: 77 },
        },
        base: {
          ref: manifest.repository.baseBranch,
          sha: manifest.repository.baseSha,
          repo: { id: 77 },
        },
      },
    ],
  };
}

function mutatePull(
  run: Record<string, unknown>,
  change: { head?: Record<string, unknown>; base?: Record<string, unknown> },
) {
  const pullRequests = run.pull_requests as Array<Record<string, unknown>>;
  const pull = pullRequests[0]!;
  return {
    ...run,
    pull_requests: [
      {
        ...pull,
        head: { ...(pull.head as Record<string, unknown>), ...change.head },
        base: { ...(pull.base as Record<string, unknown>), ...change.base },
      },
    ],
  };
}

function admissionFetch(
  manifest: ReturnType<typeof proposalFixture>['proposal']['manifest'],
  overrides: {
    checkRuns?: Record<string, unknown>[];
    workflowRuns?: Record<string, unknown>[];
    candidateDefinitionSha?: string;
    pull?: Record<string, unknown>;
  } = {},
) {
  const fixture = admissionFixture(manifest);
  const checkRuns = overrides.checkRuns ?? fixture.checkRuns;
  const workflowRuns = overrides.workflowRuns ?? fixture.workflowRuns;
  const policies = [
    workflowPolicy(101, '.github/workflows/test.yml'),
    workflowPolicy(102, '.github/workflows/security.yml'),
  ];
  return async (input: string | URL | Request): Promise<Response> => {
    const url = String(input);
    if (url.endsWith('/installation')) return Response.json(updaterInstallation());
    if (url.endsWith('/access_tokens')) return Response.json(updaterToken());
    if (url.includes('/git/ref/heads/')) return Response.json(refBody(url, manifest));
    if (url.endsWith('/pulls/42')) return Response.json(overrides.pull ?? openPull(manifest));
    if (url.includes('/check-runs')) {
      return Response.json({ total_count: checkRuns.length, check_runs: checkRuns });
    }
    const policy = policies.find((item) => url.includes(`/actions/workflows/${item.workflowId}`));
    if (policy && url.includes('/runs?')) {
      const runs = workflowRuns.filter((run) => run.workflow_id === policy.workflowId);
      return Response.json({ total_count: runs.length, workflow_runs: runs });
    }
    if (policy && url.endsWith(`/actions/workflows/${policy.workflowId}`)) {
      return Response.json({ id: policy.workflowId, path: policy.workflowPath, state: 'active' });
    }
    if (url.includes('/contents/')) {
      const path = policies.find((item) => url.includes(item.workflowPath))?.workflowPath;
      const candidate = url.includes(`ref=${manifest.candidateSha}`);
      return Response.json({
        type: 'file',
        path,
        sha: candidate ? (overrides.candidateDefinitionSha ?? 'd'.repeat(40)) : 'd'.repeat(40),
      });
    }
    throw new Error(`Unexpected request: ${url}`);
  };
}
