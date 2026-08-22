import { generateKeyPairSync } from 'node:crypto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { GitHubAppGateway } from './github.js';
import { proposalFixture } from './test-utils.js';

describe('GitHub App gateway', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('requires every named check or commit status to report success', async () => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json({ id: 9 });
        if (url.endsWith('/access_tokens')) {
          return Response.json({ token: 'installation-token', expires_at: '2027-01-01T00:00:00Z' });
        }
        if (url.includes('/git/ref/heads/')) {
          return Response.json({ object: { sha: manifest.candidateSha } });
        }
        if (url.includes('/check-runs')) {
          return Response.json({
            check_runs: [
              { id: 1, name: 'test', status: 'completed', conclusion: 'failure' },
              { id: 2, name: 'test', status: 'completed', conclusion: 'success' },
            ],
          });
        }
        if (url.includes('/status?')) {
          return Response.json({ statuses: [{ id: 4, context: 'security', state: 'success' }] });
        }
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().requiredChecks(manifest)).resolves.toEqual({
      status: 'passed',
      checks: { test: 'success', security: 'success' },
    });
  });

  it('treats an absent required check as pending', async () => {
    const manifest = proposalFixture().proposal.manifest;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json({ id: 9 });
        if (url.endsWith('/access_tokens')) {
          return Response.json({ token: 'installation-token', expires_at: '2027-01-01T00:00:00Z' });
        }
        if (url.includes('/git/ref/heads/')) {
          return Response.json({ object: { sha: manifest.candidateSha } });
        }
        if (url.includes('/check-runs')) {
          return Response.json({
            check_runs: [{ id: 1, name: 'test', status: 'completed', conclusion: 'success' }],
          });
        }
        if (url.includes('/status?')) return Response.json({ statuses: [] });
        throw new Error(`Unexpected request: ${url}`);
      }),
    );
    await expect(createGateway().requiredChecks(manifest)).resolves.toEqual({
      status: 'pending',
      checks: { test: 'success', security: 'missing' },
    });
  });

  it('creates a PR only after confirming the branch SHA and embeds the manifest receipt', async () => {
    const proposal = proposalFixture().proposal;
    const manifest = proposal.manifest;
    let createBody: Record<string, unknown> | null = null;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json({ id: 9 });
        if (url.endsWith('/access_tokens')) {
          return Response.json({ token: 'installation-token', expires_at: '2027-01-01T00:00:00Z' });
        }
        if (url.includes('/git/ref/heads/')) {
          return Response.json({ object: { sha: manifest.candidateSha } });
        }
        if (url.includes('/pulls?')) return Response.json([]);
        if (url.endsWith('/pulls') && init?.method === 'POST') {
          createBody = JSON.parse(String(init.body));
          return Response.json({
            number: 42,
            html_url: 'https://github.com/mizuki-labs/mizuki/pull/42',
            state: 'open',
            head: { sha: manifest.candidateSha },
            base: { ref: manifest.repository.baseBranch },
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
  });

  it('returns the existing merge receipt after a crash instead of merging twice', async () => {
    const manifest = proposalFixture().proposal.manifest;
    let mergeRequests = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        const url = String(input);
        if (url.endsWith('/installation')) return Response.json({ id: 9 });
        if (url.endsWith('/access_tokens')) {
          return Response.json({ token: 'installation-token', expires_at: '2027-01-01T00:00:00Z' });
        }
        if (url.includes('/git/ref/heads/')) {
          return Response.json({ object: { sha: manifest.candidateSha } });
        }
        if (url.endsWith('/pulls/42') && init?.method !== 'PUT') {
          return Response.json({
            number: 42,
            html_url: 'https://github.com/mizuki-labs/mizuki/pull/42',
            state: 'closed',
            merged_at: '2026-08-22T12:10:00Z',
            merge_commit_sha: 'b'.repeat(40),
            head: { sha: manifest.candidateSha },
            base: { ref: manifest.repository.baseBranch },
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
});

function createGateway(): GitHubAppGateway {
  const pair = generateKeyPairSync('rsa', { modulusLength: 2048 });
  return new GitHubAppGateway({
    apiUrl: 'https://api.github.test',
    appId: 123,
    privateKey: pair.privateKey.export({ type: 'pkcs8', format: 'pem' }).toString(),
    timeoutMs: 1_000,
    mergeMethod: 'squash',
  });
}
