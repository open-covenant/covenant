import { describe, expect, it, vi } from 'vitest';
import { GitHubMergeVerifier, type MergeVerificationRequest } from './github.js';

const request: MergeVerificationRequest = {
  repository: 'owner/repository',
  issueNumber: 17,
  claimantGitHubLogin: 'contributor',
  pullRequestNumber: 23,
  authorizedAt: new Date('2026-08-22T12:00:00.000Z'),
};

describe('independent GitHub merge verification', () => {
  it('verifies repository-scoped merge evidence for liability discharge', async () => {
    const { verifier } = fixture(responseBody());
    await expect(
      verifier.verifyRepositoryMerge({
        repository: 'owner/repository',
        pullRequestNumber: 23,
      }),
    ).resolves.toEqual({
      repository: 'owner/repository',
      pullRequestNumber: 23,
      pullRequestUrl: 'https://github.com/owner/repository/pull/23',
      mergeCommitOid: 'a'.repeat(40),
      createdAt: '2026-08-22T12:01:00.000Z',
      mergedAt: '2026-08-22T12:05:00.000Z',
    });
  });

  it('rejects discharge evidence targeting a different base repository', async () => {
    const body = responseBody();
    body.data.repository.pullRequest.baseRepository.nameWithOwner = 'attacker/repository';
    await expect(
      fixture(body).verifier.verifyRepositoryMerge({
        repository: 'owner/repository',
        pullRequestNumber: 23,
      }),
    ).rejects.toMatchObject({ code: 'github_repository_mismatch' });
  });

  it('accepts a merged claimant PR that closes the immutable issue', async () => {
    const { verifier, fetcher } = fixture(responseBody());
    await expect(verifier.verify(request)).resolves.toEqual({
      repository: 'owner/repository',
      issueNumber: 17,
      claimantGitHubLogin: 'contributor',
      pullRequestNumber: 23,
      pullRequestUrl: 'https://github.com/owner/repository/pull/23',
      mergeCommitOid: 'a'.repeat(40),
      createdAt: '2026-08-22T12:01:00.000Z',
      mergedAt: '2026-08-22T12:05:00.000Z',
    });

    expect(fetcher).toHaveBeenCalledOnce();
    const [url, init] = fetcher.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('https://api.github.com/graphql');
    expect(init.redirect).toBe('error');
    expect(init.headers).toMatchObject({
      authorization: 'Bearer github-read-only-test-token',
      'x-github-api-version': '2022-11-28',
    });
  });

  it('rejects an unmerged PR', async () => {
    const body = responseBody();
    const pullRequest = body.data.repository.pullRequest;
    pullRequest.state = 'OPEN';
    pullRequest.merged = false;
    pullRequest.mergedAt = null;
    pullRequest.mergeCommit = null;
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_pr_not_merged',
    });
  });

  it('rejects a merged PR targeting a different repository', async () => {
    const body = responseBody();
    body.data.repository.pullRequest.baseRepository.nameWithOwner = 'attacker/repository';
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_repository_mismatch',
    });
  });

  it('rejects evidence from a non-public repository', async () => {
    const body = responseBody();
    body.data.repository.visibility = 'PRIVATE';
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_repository_not_public',
    });
  });

  it('rejects a PR authored by a different account', async () => {
    const body = responseBody();
    body.data.repository.pullRequest.author.login = 'attacker';
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_claimant_mismatch',
    });
  });

  it('rejects a PR that does not close the immutable issue', async () => {
    const body = responseBody();
    body.data.repository.pullRequest.closingIssuesReferences.nodes[0].number = 99;
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_issue_mismatch',
    });
  });

  it('rejects an old merged PR recycled after escrow authorization', async () => {
    const body = responseBody();
    body.data.repository.pullRequest.createdAt = '2026-08-22T11:59:59.000Z';
    await expect(fixture(body).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_pr_too_old',
    });
  });

  it('fails closed when GitHub is unavailable', async () => {
    const fetcher = vi.fn(async () => {
      throw new Error('network unavailable');
    }) as unknown as typeof fetch;
    const verifier = new GitHubMergeVerifier('github-read-only-test-token', fetcher);
    await expect(verifier.verify(request)).rejects.toMatchObject({
      code: 'github_unavailable',
      retryable: true,
    });
  });

  it('fails closed on GraphQL errors and malformed evidence', async () => {
    await expect(
      fixture({ data: null, errors: [{ message: 'forbidden' }] }).verifier.verify(request),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
    await expect(
      fixture({
        data: { repository: { visibility: 'PUBLIC', pullRequest: null } },
      }).verifier.verify(request),
    ).rejects.toMatchObject({ code: 'github_pr_not_found' });
  });
});

describe('claimant OAuth verification', () => {
  it('returns only the canonical identity controlled by the OAuth token', async () => {
    const { verifier } = fixture({
      id: 123,
      login: 'Contributor',
    });
    await expect(verifier.verifyOauthIdentity('o'.repeat(20))).resolves.toEqual({
      login: 'contributor',
      githubId: '123',
    });
  });

  it('rejects malformed identity evidence', async () => {
    await expect(
      fixture({ id: 0, login: '' }).verifier.verifyOauthIdentity('o'.repeat(20)),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
  });
});

function fixture(body: object) {
  const fetcher = vi.fn(
    async () =>
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
  ) as unknown as ReturnType<typeof vi.fn> & typeof fetch;
  return {
    fetcher,
    verifier: new GitHubMergeVerifier('github-read-only-test-token', fetcher),
  };
}

function responseBody() {
  return {
    data: {
      repository: {
        visibility: 'PUBLIC' as 'PUBLIC' | 'PRIVATE' | 'INTERNAL',
        pullRequest: {
          number: 23,
          url: 'https://github.com/owner/repository/pull/23',
          state: 'MERGED',
          merged: true,
          mergedAt: '2026-08-22T12:05:00.000Z' as string | null,
          createdAt: '2026-08-22T12:01:00.000Z',
          mergeCommit: { oid: 'a'.repeat(40) } as { oid: string } | null,
          author: { login: 'contributor' },
          baseRepository: { nameWithOwner: 'owner/repository' },
          closingIssuesReferences: {
            nodes: [
              {
                number: 17,
                repository: { nameWithOwner: 'owner/repository' },
              },
            ],
          },
        },
      },
    },
  };
}
