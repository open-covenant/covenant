import { generateKeyPairSync } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { loadConfig } from './config.js';
import { assertIssueAuthorized, GithubClient } from './github.js';

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
