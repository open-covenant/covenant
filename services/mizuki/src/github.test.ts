import { createHash, generateKeyPairSync } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { assertIssueAuthorized, deliveryDiffHash, GithubClient } from './github.js';
import type { Job, RunArtifacts } from './types.js';

describe('GitHub issue authorization', () => {
  it('requires the repository-controlled authorization label', () => {
    expect(() => assertIssueAuthorized(['bug'], 'mizuki:authorized')).toThrow('issue must have');
    expect(() => assertIssueAuthorized(['Mizuki:Authorized'], 'mizuki:authorized')).not.toThrow();
  });
});

describe('GitHub issue admission', () => {
  it('rejects pull requests passed through the Issues API', async () => {
    const request = async (input: string | URL | Request) => {
      const path = new URL(String(input)).pathname;
      if (path.endsWith('/repos/example/project')) {
        return Response.json({ private: false, default_branch: 'main' });
      }
      if (path.endsWith('/repos/example/project/issues/7')) {
        return Response.json({
          title: 'Existing pull request',
          body: null,
          labels: [],
          pull_request: { url: 'https://api.github.com/repos/example/project/pulls/7' },
        });
      }
      if (path.endsWith('/repos/example/project/contents')) return Response.json([]);
      throw new Error(`unexpected request: ${path}`);
    };
    const github = new GithubClient(
      loadConfig({ MIZUKI_REQUIRE_GITHUB_APP: '0', MIZUKI_PAYMENT_MODE: 'mock' }),
      request as typeof fetch,
    );
    await expect(github.issue('https://github.com/example/project/issues/7')).rejects.toThrow(
      'pull request URLs cannot be submitted',
    );
  });

  it('binds the label event and current maintainer permission into the quote', async () => {
    const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
    let actor = { id: 42, login: 'maintainer', type: 'User' };
    let issueTitle = 'Fix README typo';
    let issueBody = 'Correct one command.';
    let issueLabels = [{ name: 'mizuki:authorized' }];
    const request = async (input: string | URL | Request) => {
      const url = new URL(String(input));
      const path = url.pathname;
      if (path === '/repos/example/project') {
        return Response.json({ private: false, default_branch: 'main' });
      }
      if (path === '/repos/example/project/issues/8') {
        return Response.json({
          title: issueTitle,
          body: issueBody,
          labels: issueLabels,
        });
      }
      if (path === '/repos/example/project/contents') return Response.json([]);
      if (path === '/repos/example/project/branches/main') {
        return Response.json({ commit: { sha: 'a'.repeat(40) } });
      }
      if (path === '/repos/example/project/installation') {
        return Response.json(coreInstallation(7));
      }
      if (path === '/app/installations/7/access_tokens') {
        return Response.json(coreToken());
      }
      if (path === '/repos/example/project/issues/8/events') {
        return Response.json([
          {
            event: 'labeled',
            created_at: '2026-08-22T10:00:00.000Z',
            label: { name: 'Mizuki:Authorized' },
            actor,
          },
        ]);
      }
      if (path.startsWith('/repos/example/project/collaborators/')) {
        return Response.json({ permission: 'maintain' });
      }
      throw new Error(`unexpected request: ${path}`);
    };
    const github = new GithubClient(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_GITHUB_APP_ID: '123',
        MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
      }),
      request as typeof fetch,
    );
    const issue = await github.issue('https://github.com/example/project/issues/8');
    expect(issue.authorizationReceipt).toMatchObject({
      actorId: '42',
      actorLogin: 'maintainer',
      permission: 'maintain',
      authorizedAt: '2026-08-22T10:00:00.000Z',
    });
    await expect(
      github.assertIssueAuthorization(
        'example',
        'project',
        8,
        7,
        issue.authorizationReceipt?.evidenceHash,
        { title: issue.title, body: issue.body },
      ),
    ).resolves.toMatchObject({ actorId: '42' });

    issueLabels = [{ name: 'mizuki:authorized' }, { name: 'enhancement' }];
    await expect(
      github.assertIssueAuthorization(
        'example',
        'project',
        8,
        7,
        issue.authorizationReceipt?.evidenceHash,
        { title: issue.title, body: issue.body },
      ),
    ).rejects.toThrow('maintenance-only scope');

    issueLabels = [{ name: 'mizuki:authorized' }];
    issueTitle = 'Add a reset button';
    await expect(
      github.assertIssueAuthorization(
        'example',
        'project',
        8,
        7,
        issue.authorizationReceipt?.evidenceHash,
        { title: issue.title, body: issue.body },
      ),
    ).rejects.toThrow('maintenance-only scope');

    issueTitle = issue.title;
    issueBody = 'Correct two commands.';
    await expect(
      github.assertIssueAuthorization(
        'example',
        'project',
        8,
        7,
        issue.authorizationReceipt?.evidenceHash,
        { title: issue.title, body: issue.body },
      ),
    ).rejects.toThrow('changed after the quote');

    issueBody = issue.body;
    actor = { id: 99, login: 'other-maintainer', type: 'User' };
    await expect(
      github.assertIssueAuthorization(
        'example',
        'project',
        8,
        7,
        issue.authorizationReceipt?.evidenceHash,
        { title: issue.title, body: issue.body },
      ),
    ).rejects.toThrow('authorization changed after the quote');
  });
});

describe('workbench repository access', () => {
  it('reads account repository metadata without fetching root contents', async () => {
    const paths: string[] = [];
    const github = reviewClient(async (input) => {
      const url = new URL(String(input));
      paths.push(url.pathname);
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/app/installations/1/access_tokens') {
        return Response.json(coreToken());
      }
      if (url.pathname === '/repos/example/project') {
        return Response.json({ private: false, default_branch: 'main' });
      }
      if (url.pathname === '/repos/example/project/collaborators/maintainer/permission') {
        return Response.json({ permission: 'maintain' });
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.repositoryMetadataForMaintainer('example', 'project', 'maintainer'),
    ).resolves.toMatchObject({ repository: 'example/project', permission: 'maintain' });
    expect(paths).not.toContain('/repos/example/project/contents');
  });

  it('requires current maintainer access and attributable authorization for eligible issues', async () => {
    const github = reviewClient(async (input) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project') {
        return Response.json({ private: false, default_branch: 'main' });
      }
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/app/installations/1/access_tokens') {
        return Response.json(coreToken());
      }
      if (url.pathname === '/repos/example/project/contents') {
        return Response.json([{ name: 'package-lock.json' }]);
      }
      if (url.pathname === '/repos/example/project/branches/main') {
        return Response.json({ commit: { sha: 'a'.repeat(40) } });
      }
      if (url.pathname === '/repos/example/project/issues' && url.searchParams.has('state')) {
        return Response.json([
          {
            number: 7,
            title: 'Fix docs typo',
            body: 'Correct docs/guide.md.',
            labels: [{ name: 'mizuki:authorized' }],
          },
          {
            number: 8,
            title: 'Fix another docs typo',
            body: 'Correct docs/other.md.',
            labels: [{ name: 'mizuki:authorized' }],
          },
          {
            number: 9,
            title: 'Fix one more docs typo',
            body: 'Correct docs/third.md.',
            labels: [{ name: 'mizuki:authorized' }],
          },
        ]);
      }
      if (url.pathname === '/repos/example/project/issues/7') {
        return Response.json({
          number: 7,
          title: 'Fix docs typo',
          body: 'Correct docs/guide.md.',
          labels: [{ name: 'mizuki:authorized' }],
        });
      }
      if (url.pathname === '/repos/example/project/issues/8') {
        return Response.json({
          number: 8,
          title: 'Fix another docs typo',
          body: 'Correct docs/other.md.',
          labels: [{ name: 'mizuki:authorized' }],
        });
      }
      if (url.pathname === '/repos/example/project/issues/9') {
        return Response.json({
          number: 9,
          title: 'Fix one more docs typo',
          body: 'Correct docs/third.md.',
          labels: [{ name: 'mizuki:authorized' }],
        });
      }
      if (url.pathname === '/repos/example/project/issues/7/events') {
        return Response.json([
          {
            event: 'labeled',
            created_at: '2026-08-25T00:00:00.000Z',
            label: { name: 'mizuki:authorized' },
            actor: { id: 42, login: 'maintainer', type: 'User' },
          },
        ]);
      }
      if (url.pathname === '/repos/example/project/issues/8/events') return Response.json([]);
      if (url.pathname === '/repos/example/project/issues/9/events') {
        return Response.json({ error: 'upstream unavailable' }, { status: 500 });
      }
      if (url.pathname.startsWith('/repos/example/project/collaborators/')) {
        return Response.json({ permission: 'maintain' });
      }
      throw new Error(`unexpected request: ${url}`);
    });

    const result = await github.issuesForMaintainer('example', 'project', 'maintainer');

    expect(result.repository).toMatchObject({
      repository: 'example/project',
      permission: 'maintain',
    });
    expect(result.issues).toEqual([
      expect.objectContaining({
        number: 7,
        authorized: true,
        eligibility: true,
        class: 'micro',
      }),
      expect.objectContaining({
        number: 8,
        authorized: false,
        eligibility: false,
        reason: 'Have a maintainer remove and reapply the mizuki:authorized label.',
      }),
      expect.objectContaining({
        number: 9,
        authorized: false,
        authorizationUnavailable: true,
        eligibility: false,
        reason: 'Issue authorization could not be verified. Try again shortly.',
      }),
    ]);
  });

  it('reports transient App verification failures without prescribing installation or relabeling', async () => {
    const github = reviewClient(async (input) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json({ error: 'upstream unavailable' }, { status: 500 });
      }
      if (url.pathname === '/repos/example/project') {
        return Response.json({ private: false, default_branch: 'main' });
      }
      if (url.pathname === '/repos/example/project/issues/7') {
        return Response.json({
          number: 7,
          title: 'Fix docs typo',
          body: 'Correct docs/guide.md.',
          labels: [{ name: 'mizuki:authorized' }],
        });
      }
      if (url.pathname === '/repos/example/project/contents') {
        return Response.json([{ name: 'package-lock.json' }]);
      }
      if (url.pathname === '/repos/example/project/branches/main') {
        return Response.json({ commit: { sha: 'a'.repeat(40) } });
      }
      throw new Error(`unexpected request: ${url}`);
    });

    const result = await github.preflightIssue(
      'https://github.com/example/project/issues/7',
      'maintainer',
    );

    expect(result.core).toEqual({ status: 'unavailable' });
    expect(result.issue).toMatchObject({
      scopeEligible: true,
      eligibility: false,
      authorizationUnavailable: true,
      reason: 'Issue authorization could not be verified. Try again shortly.',
    });
    expect(result.blockers.join(' ')).not.toMatch(/install|reapply/i);
  });
});

describe('pull request publication recovery', () => {
  it('reuses only a pull request whose head is the checkpointed delivery commit', async () => {
    const expectedHead = 'd'.repeat(40);
    const events: string[] = [];
    const github = publicationClient(expectedHead, events);

    await expect(
      github.publish(publicationJob(expectedHead), emptyArtifacts, async () => {
        events.push('binding');
      }),
    ).resolves.toBe('https://github.com/example/project/pull/31');
    expect(events[0]).toBe('binding');
    expect(events).toContain('branch');
  });

  it('rejects a branch-matching pull request whose head was substituted', async () => {
    const expectedHead = 'd'.repeat(40);
    const github = publicationClient('e'.repeat(40));

    await expect(github.publish(publicationJob(expectedHead), emptyArtifacts)).rejects.toThrow(
      'GitHub request failed with HTTP 422',
    );
  });

  it('retries evidence collection while GitHub stabilizes merge metadata', async () => {
    const expectedHead = 'd'.repeat(40);
    const events: string[] = [];
    const github = publicationClient(expectedHead, events, 'transient');

    await expect(github.publish(publicationJob(expectedHead), emptyArtifacts)).resolves.toBe(
      'https://github.com/example/project/pull/31',
    );
    expect(events.filter((event) => event === 'metadata')).toHaveLength(4);
    expect(events.filter((event) => event === 'diff')).toHaveLength(2);
    expect(events.filter((event) => event === 'checks')).toHaveLength(2);
    expect(events.filter((event) => event === 'files')).toHaveLength(2);
  });

  it('stops after one retry when merge metadata keeps changing', async () => {
    const expectedHead = 'd'.repeat(40);
    const events: string[] = [];
    const github = publicationClient(expectedHead, events, 'persistent');

    await expect(github.publish(publicationJob(expectedHead), emptyArtifacts)).rejects.toThrow(
      'pull request changed while review evidence was collected',
    );
    expect(events.filter((event) => event === 'metadata')).toHaveLength(4);
    expect(events.filter((event) => event === 'diff')).toHaveLength(2);
  });

  it('ignores only index object abbreviation length in delivery evidence', async () => {
    const expectedHead = 'd'.repeat(40);
    const reviewed =
      'diff --git a/docs/guide.md b/docs/guide.md\n' +
      'index adc7fc1..a3f8a51 100644\n' +
      '--- a/docs/guide.md\n' +
      '+++ b/docs/guide.md\n' +
      '@@ -1 +1 @@\n' +
      '-old\n' +
      '+new\n';
    const published = reviewed.replace('adc7fc1..a3f8a51', 'adc7fc169..a3f8a5189');
    const github = publicationClient(expectedHead, [], false, published);

    expect(deliveryDiffHash(reviewed)).toBe(
      '8ceb7d8b40d653eaa60db13f165834810c44fb6e3615cfd0433ac5f4b2c3eddf',
    );

    await expect(
      github.publish(publicationJob(expectedHead), { ...emptyArtifacts, patch: reviewed }),
    ).resolves.toBe('https://github.com/example/project/pull/31');
  });

  it('rejects changed diff content after independent review', async () => {
    const expectedHead = 'd'.repeat(40);
    const reviewed =
      'diff --git a/docs/guide.md b/docs/guide.md\n' +
      'index adc7fc1..a3f8a51 100644\n' +
      '--- a/docs/guide.md\n' +
      '+++ b/docs/guide.md\n' +
      '@@ -1 +1 @@\n' +
      '-old\n' +
      '+new\n';
    const published = reviewed
      .replace('adc7fc1..a3f8a51', 'adc7fc169..a3f8a5189')
      .replace('+new', '+different');
    const github = publicationClient(expectedHead, [], false, published);

    await expect(
      github.publish(publicationJob(expectedHead), { ...emptyArtifacts, patch: reviewed }),
    ).rejects.toThrow('published pull request does not match the reviewed delivery artifact');
  });

  it('preserves file modes and non-header content in the delivery hash', () => {
    const reviewed =
      'diff --git a/docs/guide.md b/docs/guide.md\n' +
      'index adc7fc1..a3f8a51 100644\n' +
      '--- a/docs/guide.md\n' +
      '+++ b/docs/guide.md\n' +
      '@@ -1 +1 @@\n' +
      '-old\n' +
      '+index deadbee..feedbee 100644\n';
    const hash = deliveryDiffHash(reviewed);

    expect(deliveryDiffHash(reviewed.replace('100644\n---', '100755\n---'))).not.toBe(hash);
    expect(deliveryDiffHash(reviewed.replace('+index deadbee', '+index badc0de'))).not.toBe(hash);
    expect(deliveryDiffHash(reviewed.replace(' 100644\n---', ' suffix\n---'))).not.toBe(hash);
  });
});

describe('pull request review evidence', () => {
  it('binds an exact head and diff between matching metadata snapshots', async () => {
    const diff = 'diff --git a/src/parser.ts b/src/parser.ts\n+handle empty input\n';
    const github = reviewClient(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === '/app/installations/1/access_tokens') {
        return Response.json(coreToken());
      }
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/repos/example/project/pulls/23') {
        if (new Headers(init?.headers).get('accept') === 'application/vnd.github.v3.diff') {
          return new Response(diff);
        }
        return Response.json(reviewPull());
      }
      if (url.pathname === `/repos/example/project/commits/${'a'.repeat(40)}/check-runs`) {
        expect(url.searchParams.get('per_page')).toBe('100');
        return Response.json({
          total_count: 1,
          check_runs: [{ status: 'completed', conclusion: 'success' }],
        });
      }
      if (url.pathname === '/repos/example/project/pulls/23/files') {
        return Response.json([
          { filename: 'src/parser.ts', status: 'modified', patch: '+handle empty input' },
        ]);
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.pullRequestReviewData('https://github.com/example/project/pull/23', 1),
    ).resolves.toMatchObject({
      headSha: 'b'.repeat(40),
      baseSha: 'd'.repeat(40),
      baseRef: 'main',
      diffHash: createHash('sha256').update(diff).digest('hex'),
      mergedAt: '2026-08-22T12:05:00.000Z',
      mergeCommitSha: 'a'.repeat(40),
      checksPassed: true,
      checkCount: 1,
    });
  });

  it('rejects a base change while diff evidence is being collected', async () => {
    let metadataReads = 0;
    const github = reviewClient(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === '/app/installations/1/access_tokens') {
        return Response.json(coreToken());
      }
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/repos/example/project/pulls/23') {
        if (new Headers(init?.headers).get('accept') === 'application/vnd.github.v3.diff') {
          return new Response('diff A');
        }
        metadataReads += 1;
        return Response.json(
          reviewPull('b'.repeat(40), metadataReads === 1 ? 'd'.repeat(40) : 'e'.repeat(40)),
        );
      }
      if (url.pathname.includes('/check-runs')) {
        return Response.json({
          total_count: 1,
          check_runs: [{ status: 'completed', conclusion: 'success' }],
        });
      }
      if (url.pathname.endsWith('/files')) {
        return Response.json([{ filename: 'src/parser.ts', status: 'modified', patch: '+A' }]);
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.pullRequestReviewData('https://github.com/example/project/pull/23', 1),
    ).rejects.toThrow('changed while review evidence was collected');
  });
});

describe('GitHub App readiness', () => {
  it('authenticates the configured App id with GitHub', async () => {
    const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
    const request = vi.fn<typeof fetch>(async (_input, init) => {
      expect(init?.headers).toMatchObject({
        authorization: expect.stringMatching(/^Bearer [^.]+\.[^.]+\.[^.]+$/),
      });
      return Response.json({ id: 123, slug: 'mizuki', permissions: corePermissions() });
    });
    const github = new GithubClient(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_GITHUB_APP_ID: '123',
        MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
      }),
      request,
    );

    await expect(github.readiness()).resolves.toBeUndefined();
    expect(request).toHaveBeenCalledWith(
      'https://api.github.com/app',
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
  });

  it('fails closed when GitHub authenticates a different App', async () => {
    const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
    const github = new GithubClient(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_GITHUB_APP_ID: '123',
        MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
      }),
      async () => Response.json({ id: 456, slug: 'other', permissions: corePermissions() }),
    );
    await expect(github.readiness()).rejects.toThrow('different App');
  });

  it('fails closed when the App permission contract drifts', async () => {
    const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
    const github = new GithubClient(
      loadConfig({
        MIZUKI_PAYMENT_MODE: 'mock',
        MIZUKI_GITHUB_APP_ID: '123',
        MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
      }),
      async () =>
        Response.json({
          id: 123,
          slug: 'mizuki',
          permissions: { ...corePermissions(), workflows: 'write' },
        }),
    );
    await expect(github.readiness()).rejects.toThrow('permissions do not match');
  });
});

describe('repository-scoped GitHub App credentials', () => {
  it('requests and accepts only the exact repository and delivery permissions', async () => {
    let tokenBody: Record<string, unknown> | undefined;
    const github = reviewClient(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/app/installations/1/access_tokens') {
        tokenBody = JSON.parse(String(init?.body));
        return Response.json(coreToken());
      }
      if (url.pathname === '/repos/example/project/pulls/23') {
        return Response.json({ merged_at: null });
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.pullRequestMergedAt('https://github.com/example/project/pull/23', 1),
    ).resolves.toBeUndefined();
    expect(tokenBody).toEqual({ repositories: ['project'], permissions: corePermissions() });
  });

  it.each([
    ['all-repository selection', { repository_selection: 'all' }],
    ['an extra permission', { permissions: { ...corePermissions(), workflows: 'read' } }],
    ['a missing permission', { permissions: { ...corePermissions(), checks: undefined } }],
    ['another repository', { repositories: [{ name: 'other', full_name: 'example/other' }] }],
  ])('rejects a token response with %s', async (_case, overrides) => {
    const github = reviewClient(async (input) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/app/installations/1/access_tokens') {
        const body = coreToken(overrides);
        if (body.permissions && typeof body.permissions === 'object') {
          body.permissions = Object.fromEntries(
            Object.entries(body.permissions).filter(([, value]) => value !== undefined),
          );
        }
        return Response.json(body);
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.pullRequestMergedAt('https://github.com/example/project/pull/23', 1),
    ).rejects.toThrow();
  });

  it.each([
    ['another App', { app_id: 456 }],
    ['all repositories', { repository_selection: 'all' }],
    [
      'an extra installation permission',
      { permissions: { ...corePermissions(), workflows: 'read' } },
    ],
  ])('rejects an installation bound to %s', async (_case, overrides) => {
    const github = reviewClient(async (input) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation(1, overrides));
      }
      throw new Error(`unexpected request: ${url}`);
    });
    await expect(
      github.pullRequestMergedAt('https://github.com/example/project/pull/23', 1),
    ).rejects.toThrow();
  });

  it('invalidates a rejected cached token and retries once', async () => {
    let mints = 0;
    const github = reviewClient(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation());
      }
      if (url.pathname === '/app/installations/1/access_tokens') {
        mints += 1;
        return Response.json(coreToken({ token: `installation-token-for-repository-${mints}` }));
      }
      if (url.pathname === '/repos/example/project/pulls/23') {
        const authorization = new Headers(init?.headers).get('authorization');
        if (authorization?.endsWith('-1')) return Response.json({}, { status: 401 });
        return Response.json({ merged_at: null });
      }
      throw new Error(`unexpected request: ${url}`);
    });

    await expect(
      github.pullRequestMergedAt('https://github.com/example/project/pull/23', 1),
    ).resolves.toBeUndefined();
    expect(mints).toBe(2);
  });
});

function reviewClient(request: typeof fetch): GithubClient {
  const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  return new GithubClient(
    loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_GITHUB_APP_ID: '123',
      MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    }),
    request,
  );
}

function reviewPull(headSha = 'b'.repeat(40), baseSha = 'd'.repeat(40)) {
  return {
    changed_files: 1,
    merged_at: '2026-08-22T12:05:00.000Z',
    merge_commit_sha: 'a'.repeat(40),
    head: { sha: headSha },
    base: { sha: baseSha, ref: 'main' },
  };
}

function corePermissions() {
  return {
    checks: 'read',
    contents: 'write',
    issues: 'read',
    metadata: 'read',
    pull_requests: 'write',
  } as const;
}

function coreInstallation(id = 1, overrides: Record<string, unknown> = {}) {
  return {
    id,
    app_id: 123,
    repository_selection: 'selected',
    permissions: corePermissions(),
    suspended_at: null,
    ...overrides,
  };
}

function coreToken(overrides: Record<string, unknown> = {}) {
  return {
    token: 'installation-token-for-one-repository',
    expires_at: new Date(Date.now() + 60 * 60_000).toISOString(),
    permissions: corePermissions(),
    repository_selection: 'selected',
    repositories: [{ name: 'project', full_name: 'example/project' }],
    ...overrides,
  };
}

const emptyArtifacts: RunArtifacts = {
  patch: '',
  changedFiles: [],
  files: [],
  validations: [],
};

function publicationClient(
  existingHead: string,
  events: string[] = [],
  mergeMetadataDrift: false | 'transient' | 'persistent' = false,
  publishedDiff = '',
): GithubClient {
  const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const expectedHead = 'd'.repeat(40);
  let metadataReads = 0;
  return new GithubClient(
    loadConfig({
      MIZUKI_PAYMENT_MODE: 'mock',
      MIZUKI_REQUIRE_GITHUB_APP: '0',
      MIZUKI_GITHUB_APP_ID: '123',
      MIZUKI_GITHUB_PRIVATE_KEY: privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    }),
    async (input, init) => {
      const url = new URL(String(input));
      const method = init?.method ?? 'GET';
      if (url.pathname === '/repos/example/project/installation') {
        return Response.json(coreInstallation(7));
      }
      if (url.pathname === '/app/installations/7/access_tokens') {
        return Response.json(coreToken());
      }
      if (url.pathname === `/repos/example/project/git/commits/${'b'.repeat(40)}`) {
        return Response.json({ tree: { sha: 'c'.repeat(40) } });
      }
      if (url.pathname === '/repos/example/project/git/refs' && method === 'POST') {
        events.push('branch');
        return Response.json({ message: 'Reference already exists' }, { status: 422 });
      }
      if (url.pathname === '/repos/example/project/git/ref/heads/mizuki%2Faaaaaaaa') {
        return Response.json({ object: { sha: expectedHead } });
      }
      if (url.pathname === '/repos/example/project/pulls' && method === 'GET') {
        return Response.json([
          {
            html_url: 'https://github.com/example/project/pull/31',
            head: { ref: 'mizuki/aaaaaaaa', sha: existingHead },
            base: { ref: 'main' },
          },
        ]);
      }
      if (url.pathname === '/repos/example/project/pulls/31') {
        if (new Headers(init?.headers).get('accept') === 'application/vnd.github.v3.diff') {
          events.push('diff');
          return new Response(publishedDiff);
        }
        events.push('metadata');
        const mergeCommitSha =
          mergeMetadataDrift === 'transient'
            ? metadataReads > 0
              ? 'a'.repeat(40)
              : null
            : mergeMetadataDrift === 'persistent' && metadataReads % 2 === 1
              ? 'a'.repeat(40)
              : null;
        metadataReads += 1;
        return Response.json({
          changed_files: publishedDiff ? 1 : 0,
          merged_at: null,
          merge_commit_sha: mergeCommitSha,
          head: { sha: expectedHead },
          base: { sha: 'b'.repeat(40), ref: 'main' },
        });
      }
      if (
        url.pathname === `/repos/example/project/commits/${expectedHead}/check-runs` ||
        url.pathname === `/repos/example/project/commits/${'a'.repeat(40)}/check-runs`
      ) {
        events.push('checks');
        return Response.json({ total_count: 0, check_runs: [] });
      }
      if (url.pathname === '/repos/example/project/pulls/31/files') {
        events.push('files');
        return Response.json(
          publishedDiff ? [{ filename: 'docs/guide.md', status: 'modified', patch: '+new' }] : [],
        );
      }
      if (url.pathname === '/repos/example/project/pulls' && method === 'POST') {
        return Response.json({ message: 'A pull request already exists' }, { status: 422 });
      }
      throw new Error(`unexpected request: ${method} ${url}`);
    },
  );
}

function publicationJob(deliveryCommitSha: string): Job {
  const timestamp = '2026-08-23T12:00:00.000Z';
  return {
    id: 'aaaaaaaa-1111-4111-8111-111111111111',
    idempotencyKey: 'publish-recovery',
    quote: {
      id: 'bbbbbbbb-1111-4111-8111-111111111111',
      issueUrl: 'https://github.com/example/project/issues/8',
      owner: 'example',
      repo: 'project',
      issueNumber: 8,
      issueTitle: 'Fix parser edge case',
      issueBody: 'Handle empty input.',
      baseSha: 'b'.repeat(40),
      defaultBranch: 'main',
      installationId: 7,
      class: 'micro',
      priceAtomic: '2000000',
      maxFiles: 3,
      maxCostUsd: 1,
      validationCommands: [],
      expiresAt: '2026-08-23T13:00:00.000Z',
    },
    payment: { payer: 'payer', transaction: 'settlement', amountAtomic: '2000000' },
    state: 'validating',
    createdAt: timestamp,
    updatedAt: timestamp,
    deliveryCommitSha,
    inputTokens: 0,
    outputTokens: 0,
    estimatedCostUsd: 0,
    version: 0,
  };
}
