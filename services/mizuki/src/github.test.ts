import { createHash, generateKeyPairSync } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { assertIssueAuthorized, GithubClient } from './github.js';
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
      if (path === '/repos/example/project/installation') return Response.json({ id: 7 });
      if (path === '/app/installations/7/access_tokens') return Response.json({ token: 'token' });
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
});

describe('pull request review evidence', () => {
  it('binds an exact head and diff between matching metadata snapshots', async () => {
    const diff = 'diff --git a/src/parser.ts b/src/parser.ts\n+handle empty input\n';
    const github = reviewClient(async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === '/app/installations/1/access_tokens') {
        return Response.json({ token: 'installation-token' });
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
        return Response.json({ token: 'installation-token' });
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
      return Response.json({ id: 123, slug: 'mizuki' });
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
      async () => Response.json({ id: 456, slug: 'other' }),
    );
    await expect(github.readiness()).rejects.toThrow('different App');
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

const emptyArtifacts: RunArtifacts = {
  patch: '',
  changedFiles: [],
  files: [],
  validations: [],
};

function publicationClient(existingHead: string, events: string[] = []): GithubClient {
  const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  const expectedHead = 'd'.repeat(40);
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
      if (url.pathname === '/app/installations/7/access_tokens') {
        return Response.json({ token: 'installation-token' });
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
          return new Response('');
        }
        return Response.json({
          changed_files: 0,
          merged_at: null,
          merge_commit_sha: null,
          head: { sha: expectedHead },
          base: { sha: 'b'.repeat(40), ref: 'main' },
        });
      }
      if (url.pathname === `/repos/example/project/commits/${expectedHead}/check-runs`) {
        return Response.json({ total_count: 0, check_runs: [] });
      }
      if (url.pathname === '/repos/example/project/pulls/31/files') {
        return Response.json([]);
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
