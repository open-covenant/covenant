import { createHash, generateKeyPairSync, verify as verifySignature } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import { GitHubMergeVerifier, type MergeVerificationRequest } from './github.js';

const APP_ID = '12345';
const NOW = Date.parse('2026-08-23T12:00:00.000Z');
const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2_048 });
const PRIVATE_KEY = privateKey.export({ format: 'pem', type: 'pkcs8' }).toString();
const pullDiff = 'diff --git a/src/parser.ts b/src/parser.ts\n+handle empty input\n';
const reviewedDiffHash = createHash('sha256').update(pullDiff).digest('hex');
const request: MergeVerificationRequest = {
  repository: 'owner/repository',
  issueNumber: 17,
  claimantGitHubLogin: 'contributor',
  pullRequestNumber: 23,
  mergeCommitSha: 'a'.repeat(40),
  reviewedHeadSha: 'b'.repeat(40),
  reviewedBaseSha: 'd'.repeat(40),
  reviewedBaseRef: 'main',
  reviewedDiffHash,
  expectedIssueTitle: 'Handle empty input',
  expectedIssueBody: 'The parser should accept an empty input.',
  maxFiles: 3,
  authorizedAt: new Date('2026-08-22T12:00:00.000Z'),
};

function repositoryMergeRequest(repository = 'owner/repository') {
  return {
    repository,
    issueNumber: 17,
    pullRequestNumber: 23,
    deliveredCommitSha: 'b'.repeat(40),
    reviewedHeadSha: 'b'.repeat(40),
    reviewedBaseSha: 'd'.repeat(40),
    reviewedBaseRef: 'main',
    reviewedDiffHash,
    notBefore: new Date('2026-08-22T12:00:00.000Z'),
  };
}

describe('independent GitHub merge verification', () => {
  it('proves a reviewed commit has no pull request before liability binding', async () => {
    const { verifier, fetcher } = fixture({ commitPulls: [] });
    await expect(
      verifier.assertCommitUnpublished('owner/repository', 'b'.repeat(40)),
    ).resolves.toBeUndefined();

    const call = fetcher.mock.calls.find(([input]) =>
      String(input).includes(`/commits/${'b'.repeat(40)}/pulls`),
    );
    expect(call?.[1]).toMatchObject({
      redirect: 'error',
      headers: {
        accept: 'application/vnd.github+json',
        authorization: expect.stringMatching(/^Bearer installation-token-/),
      },
    });
  });

  it('rejects a delivery commit that already belongs to a pull request', async () => {
    await expect(
      fixture({
        commitPulls: [{ number: 23, html_url: 'https://github.com/owner/repository/pull/23' }],
      }).verifier.assertCommitUnpublished('owner/repository', 'b'.repeat(40)),
    ).rejects.toMatchObject({ code: 'github_delivery_already_published', retryable: false });
  });

  it('verifies repository-scoped merge evidence for liability discharge', async () => {
    const { verifier } = fixture();
    await expect(verifier.verifyRepositoryMerge(repositoryMergeRequest())).resolves.toEqual({
      repository: 'owner/repository',
      issueNumber: 17,
      pullRequestNumber: 23,
      pullRequestUrl: 'https://github.com/owner/repository/pull/23',
      mergeCommitOid: 'a'.repeat(40),
      headCommitOid: 'b'.repeat(40),
      baseCommitOid: 'd'.repeat(40),
      baseRefName: 'main',
      diffHash: reviewedDiffHash,
      createdAt: '2026-08-22T12:01:00.000Z',
      mergedAt: '2026-08-22T12:05:00.000Z',
    });
  });

  it('rejects discharge evidence targeting a different base repository', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.baseRepository.nameWithOwner = 'attacker/repository';
    await expect(
      fixture({ graph }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_repository_mismatch' });
  });

  it('rejects approve-A/merge-B and an unrelated issue during liability discharge', async () => {
    const substituted = responseBody();
    substituted.data.repository.pullRequest.headRefOid = 'c'.repeat(40);
    await expect(
      fixture({ graph: substituted }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_review_mismatch' });

    const unrelated = responseBody();
    unrelated.data.repository.pullRequest.closingIssuesReferences.nodes[0]!.number = 99;
    await expect(
      fixture({ graph: unrelated }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_issue_mismatch' });
  });

  it('accepts a merged claimant PR that closes the immutable issue', async () => {
    const { verifier, fetcher } = fixture();
    await expect(verifier.verify(request)).resolves.toEqual({
      evidence: {
        repository: 'owner/repository',
        issueNumber: 17,
        claimantGitHubLogin: 'contributor',
        pullRequestNumber: 23,
        pullRequestUrl: 'https://github.com/owner/repository/pull/23',
        mergeCommitOid: 'a'.repeat(40),
        headCommitOid: 'b'.repeat(40),
        baseCommitOid: 'd'.repeat(40),
        baseRefName: 'main',
        diffHash: reviewedDiffHash,
        approvedReviewer: 'maintainer',
        approvedReviewSubmittedAt: '2026-08-22T12:04:00.000Z',
        checkCount: 2,
        createdAt: '2026-08-22T12:01:00.000Z',
        mergedAt: '2026-08-22T12:05:00.000Z',
      },
      artifact: {
        issueTitle: 'Handle empty input',
        issueBody: 'The parser should accept an empty input.',
        changedFiles: 1,
        diff: pullDiff,
      },
    });

    const evidenceCalls = fetcher.mock.calls.filter(([input]) =>
      ['/graphql', '/pulls/23'].some((path) => String(input).includes(path)),
    );
    expect(evidenceCalls).toHaveLength(4);
    for (const [, init] of evidenceCalls) {
      expect(init).toMatchObject({ redirect: 'error' });
      expect((init as RequestInit).headers).toMatchObject({
        authorization: expect.stringMatching(/^Bearer installation-token-/),
        'x-github-api-version': '2022-11-28',
      });
    }
  });

  it('rejects a merged head that differs from the reviewed revision', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.headRefOid = 'c'.repeat(40);
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_review_mismatch',
    });
  });

  it('rejects a merged diff that differs from the reviewed artifact', async () => {
    await expect(
      fixture({
        diff: 'diff --git a/src/parser.ts b/src/parser.ts\n+different change\n',
      }).verifier.verify(request),
    ).rejects.toMatchObject({ code: 'github_review_diff_mismatch' });
  });

  it('uses immutable merge parents when the base branch advances after merge', async () => {
    const confirmation = responseBody();
    confirmation.data.repository.pullRequest.baseRefOid = 'e'.repeat(40);
    await expect(fixture({ confirmation }).verifier.verify(request)).resolves.toMatchObject({
      evidence: { baseCommitOid: 'd'.repeat(40) },
    });
  });

  it('rejects mutable policy evidence that changes during artifact verification', async () => {
    const privateRepository = responseBody();
    privateRepository.data.repository.visibility = 'PRIVATE';
    await expect(
      fixture({ confirmation: privateRepository }).verifier.verify(request),
    ).rejects.toMatchObject({ code: 'github_evidence_changed' });

    const unlinkedIssue = responseBody();
    unlinkedIssue.data.repository.pullRequest.closingIssuesReferences.nodes = [];
    await expect(
      fixture({ confirmation: unlinkedIssue }).verifier.verify(request),
    ).rejects.toMatchObject({ code: 'github_evidence_changed' });
  });

  it('rejects merge ancestry that cannot bind the reviewed base and head', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.mergeCommit!.parents.nodes = [
      { oid: 'e'.repeat(40) },
      { oid: 'b'.repeat(40) },
    ];
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_merge_lineage_mismatch',
    });
  });

  it('rejects an unmerged PR', async () => {
    const graph = responseBody();
    const pullRequest = graph.data.repository.pullRequest;
    pullRequest.state = 'OPEN';
    pullRequest.merged = false;
    pullRequest.mergedAt = null;
    pullRequest.mergeCommit = null;
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_pr_not_merged',
    });
  });

  it('rejects a merged PR targeting a different repository', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.baseRepository.nameWithOwner = 'attacker/repository';
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_repository_mismatch',
    });
  });

  it('rejects evidence from a non-public repository', async () => {
    const graph = responseBody();
    graph.data.repository.visibility = 'PRIVATE';
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_repository_not_public',
    });
  });

  it('rejects a PR authored by a different account', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.author.login = 'attacker';
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_claimant_mismatch',
    });
  });

  it('rejects a PR that does not close the immutable issue', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.closingIssuesReferences.nodes[0]!.number = 99;
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_issue_mismatch',
    });
  });

  it('rejects an old merged PR recycled after escrow authorization', async () => {
    const graph = responseBody();
    graph.data.repository.pullRequest.createdAt = '2026-08-22T11:59:59.000Z';
    await expect(fixture({ graph }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_pr_too_old',
    });
  });

  it('fails closed without exposing transport errors', async () => {
    const fetcher = vi.fn(async () => {
      throw new Error('network failed with secret-installation-token');
    }) as unknown as typeof fetch;
    const verifier = new GitHubMergeVerifier({ appId: APP_ID, privateKey: PRIVATE_KEY }, fetcher);
    const failure = verifier.verify(request);
    await expect(failure).rejects.toMatchObject({ code: 'github_unavailable', retryable: true });
    await expect(failure).rejects.not.toThrow(/secret-installation-token/);
  });

  it('fails closed on GraphQL errors and malformed evidence', async () => {
    await expect(
      fixture({ graph: { data: null, errors: [{ message: 'forbidden' }] } }).verifier.verify(
        request,
      ),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
    const missingPull = responseBody();
    missingPull.data.repository.pullRequest = null as never;
    await expect(fixture({ graph: missingPull }).verifier.verify(request)).rejects.toMatchObject({
      code: 'github_pr_not_found',
    });
  });
});

describe('claimant OAuth verification', () => {
  it('uses only the caller token and returns its canonical identity', async () => {
    const { verifier, fetcher } = fixture({ user: { id: 123, login: 'Contributor' } });
    await expect(verifier.verifyOauthIdentity('caller-oauth-access-token')).resolves.toEqual({
      login: 'contributor',
      githubId: '123',
    });

    expect(fetcher).toHaveBeenCalledOnce();
    const [url, init] = fetcher.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('https://api.github.com/user');
    expect(init.headers).toMatchObject({ authorization: 'Bearer caller-oauth-access-token' });
  });

  it('rejects malformed identity evidence', async () => {
    await expect(
      fixture({ user: { id: 0, login: '' } }).verifier.verifyOauthIdentity('o'.repeat(20)),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
  });
});

describe('GitHub App credentials', () => {
  it('proves fresh exact-repository readiness with the read-only verifier App', async () => {
    const { verifier, fetcher } = fixture();

    await expect(verifier.repositoryReadiness('Owner/Repository')).resolves.toEqual({
      ready: true,
      repository: 'owner/repository',
      verifierAppId: APP_ID,
      installationId: 777,
      repositorySelection: 'selected',
      permissions: readPermissions(),
      tokenRepositories: 1,
      tokenExpiresAt: new Date(NOW + 60 * 60_000).toISOString(),
    });
    expect(fetcher.mock.calls.map(([input]) => String(input))).toEqual([
      'https://api.github.com/app',
      'https://api.github.com/repos/owner/repository/installation',
      'https://api.github.com/app/installations/777/access_tokens',
    ]);
  });

  it('fails repository readiness for invalid, absent, suspended, or wrong-App installations', async () => {
    const { suspended_at: _suspendedAt, ...missingSuspensionEvidence } = installationBody();
    await expect(fixture().verifier.repositoryReadiness('invalid')).rejects.toMatchObject({
      code: 'github_repository_invalid',
    });
    await expect(
      fixture({ installationStatus: 404 }).verifier.repositoryReadiness('owner/repository'),
    ).rejects.toMatchObject({ code: 'github_app_not_installed' });
    await expect(
      fixture({
        installation: {
          ...installationBody(),
          suspended_at: '2026-08-23T11:00:00.000Z',
        },
      }).verifier.repositoryReadiness('owner/repository'),
    ).rejects.toMatchObject({ code: 'github_app_not_installed' });
    await expect(
      fixture({ installation: missingSuspensionEvidence }).verifier.repositoryReadiness(
        'owner/repository',
      ),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ app: appBody(99999) }).verifier.repositoryReadiness('owner/repository'),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({
        installation: { ...installationBody(), app_id: 99999 },
      }).verifier.repositoryReadiness('owner/repository'),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
  });

  it('fails repository readiness for permission or one-repository scope drift', async () => {
    await expect(
      fixture({
        installation: installationBody({ actions: 'read' }),
      }).verifier.repositoryReadiness('owner/repository'),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ tokenPermissions: { issues: 'write' } }).verifier.repositoryReadiness(
        'owner/repository',
      ),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ tokenRepository: 'attacker/repository' }).verifier.repositoryReadiness(
        'owner/repository',
      ),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
  });

  it('signs a bounded RS256 JWT and verifies the configured App at /app', async () => {
    const { verifier, fetcher } = fixture();
    await expect(verifier.health()).resolves.toBeUndefined();

    const [url, init] = fetcher.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('https://api.github.com/app');
    expect(init).toMatchObject({ redirect: 'error' });
    const jwt = String((init.headers as Record<string, string>).authorization).slice(7);
    const [encodedHeader, encodedPayload, encodedSignature] = jwt.split('.');
    expect(JSON.parse(Buffer.from(encodedHeader!, 'base64url').toString())).toEqual({
      alg: 'RS256',
      typ: 'JWT',
    });
    expect(JSON.parse(Buffer.from(encodedPayload!, 'base64url').toString())).toEqual({
      iat: Math.floor(NOW / 1_000) - 60,
      exp: Math.floor(NOW / 1_000) + 540,
      iss: APP_ID,
    });
    expect(
      verifySignature(
        'RSA-SHA256',
        Buffer.from(`${encodedHeader}.${encodedPayload}`),
        publicKey,
        Buffer.from(encodedSignature!, 'base64url'),
      ),
    ).toBe(true);
  });

  it('rejects a valid response for a different App', async () => {
    await expect(fixture({ app: appBody(99999) }).verifier.health()).rejects.toMatchObject({
      code: 'github_credential_invalid',
      retryable: false,
    });
  });

  it('rejects App or installation permission expansion', async () => {
    await expect(
      fixture({ app: appBody(Number(APP_ID), { issues: 'write' }) }).verifier.health(),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ app: appBody(Number(APP_ID), { actions: 'read' }) }).verifier.health(),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({
        installation: installationBody({ pull_requests: 'write' }),
      }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({
        installation: installationBody({ actions: 'read' }),
      }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
  });

  it('rejects an installation belonging to a different App', async () => {
    await expect(
      fixture({
        installation: { ...installationBody(), app_id: 99999 },
      }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
  });

  it('rejects an all-repository installation', async () => {
    await expect(
      fixture({
        installation: { ...installationBody(), repository_selection: 'all' },
      }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid', retryable: false });
  });

  it('fails closed when the App is not installed on an external maintainer repository', async () => {
    const { verifier, fetcher } = fixture({ installationStatus: 404 });
    await expect(
      verifier.verifyRepositoryMerge(repositoryMergeRequest('external-maintainer/public-repo')),
    ).rejects.toMatchObject({ code: 'github_app_not_installed', retryable: false });
    expect(fetcher.mock.calls[0]![0]).toBe(
      'https://api.github.com/repos/external-maintainer/public-repo/installation',
    );
  });

  it('mints a token scoped to exactly one external repository and no write permission', async () => {
    const repository = 'external-maintainer/public-repo';
    const { verifier, fetcher } = fixture({
      repository,
      graph: responseBody(repository),
    });
    await verifier.verifyRepositoryMerge(repositoryMergeRequest(repository));

    const mintCall = fetcher.mock.calls.find(([input]) =>
      String(input).includes('/access_tokens'),
    )!;
    expect(mintCall[0]).toBe('https://api.github.com/app/installations/777/access_tokens');
    expect(JSON.parse(String((mintCall[1] as RequestInit).body))).toEqual({
      repositories: ['public-repo'],
      permissions: {
        checks: 'read',
        contents: 'read',
        issues: 'read',
        metadata: 'read',
        pull_requests: 'read',
        statuses: 'read',
      },
    });
  });

  it('rejects a token response scoped to another repository or with write permission', async () => {
    await expect(
      fixture({ tokenRepository: 'attacker/repository' }).verifier.verifyRepositoryMerge(
        repositoryMergeRequest(),
      ),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ tokenPermissions: { issues: 'write' } }).verifier.verifyRepositoryMerge(
        repositoryMergeRequest(),
      ),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
  });

  it('refreshes the cached repository token only inside the bounded expiry window', async () => {
    const clock = { value: NOW };
    const { verifier, fetcher } = fixture({ clock });
    const merge = repositoryMergeRequest();

    await verifier.verifyRepositoryMerge(merge);
    clock.value += 54 * 60_000;
    await verifier.verifyRepositoryMerge(merge);
    expect(mintCalls(fetcher)).toHaveLength(1);

    clock.value += 2 * 60_000;
    await verifier.verifyRepositoryMerge(merge);
    expect(mintCalls(fetcher)).toHaveLength(2);
  });

  it('does not persist installation tokens across signer restarts', async () => {
    const first = fixture();
    await first.verifier.verifyRepositoryMerge(repositoryMergeRequest());
    expect(mintCalls(first.fetcher)).toHaveLength(1);

    const restarted = fixture();
    await restarted.verifier.verifyRepositoryMerge(repositoryMergeRequest());
    expect(mintCalls(restarted.fetcher)).toHaveLength(1);
  });

  it('invalidates a rejected cached token and retries exactly once with a fresh token', async () => {
    let rejected = false;
    const { verifier, fetcher } = fixture({
      evidenceResponse: (_url, init, fallback) => {
        const authorization = (init.headers as Record<string, string>).authorization;
        if (!rejected && authorization.includes('installation-token-1-')) {
          rejected = true;
          return new Response(null, { status: 401 });
        }
        return fallback();
      },
    });

    await expect(verifier.verifyRepositoryMerge(repositoryMergeRequest())).resolves.toMatchObject({
      repository: 'owner/repository',
    });
    expect(mintCalls(fetcher)).toHaveLength(2);
    const graphCalls = fetcher.mock.calls.filter(([input]) => String(input).endsWith('/graphql'));
    expect(graphCalls).toHaveLength(3);
    expect((graphCalls[0]![1] as RequestInit).headers).toMatchObject({
      authorization: expect.stringContaining('installation-token-1-'),
    });
    expect((graphCalls[1]![1] as RequestInit).headers).toMatchObject({
      authorization: expect.stringContaining('installation-token-2-'),
    });
    expect((graphCalls[2]![1] as RequestInit).headers).toMatchObject({
      authorization: expect.stringContaining('installation-token-2-'),
    });
  });

  it('does not retry a second 401 or retry non-authentication failures', async () => {
    const unauthorized = fixture({ evidenceStatus: 401 });
    await expect(
      unauthorized.verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_unavailable' });
    expect(mintCalls(unauthorized.fetcher)).toHaveLength(2);
    expect(
      unauthorized.fetcher.mock.calls.filter(([input]) => String(input).endsWith('/graphql')),
    ).toHaveLength(2);

    const forbidden = fixture({ evidenceStatus: 403 });
    await expect(
      forbidden.verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_unavailable' });
    expect(mintCalls(forbidden.fetcher)).toHaveLength(1);
    expect(
      forbidden.fetcher.mock.calls.filter(([input]) => String(input).endsWith('/graphql')),
    ).toHaveLength(1);
  });

  it('rejects expired, implausibly long, malformed, and oversized token responses', async () => {
    await expect(
      fixture({ tokenTtlMs: 4 * 60_000 }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ tokenTtlMs: 66 * 60_000 }).verifier.verifyRepositoryMerge(repositoryMergeRequest()),
    ).rejects.toMatchObject({ code: 'github_credential_invalid' });
    await expect(
      fixture({ tokenContentType: 'text/plain' }).verifier.verifyRepositoryMerge(
        repositoryMergeRequest(),
      ),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
    await expect(
      fixture({ tokenBodyOverride: 'x'.repeat(65_537) }).verifier.verifyRepositoryMerge(
        repositoryMergeRequest(),
      ),
    ).rejects.toMatchObject({ code: 'github_invalid_response' });
  });
});

interface FixtureOptions {
  repository?: string;
  graph?: object;
  confirmation?: object;
  user?: object;
  app?: object;
  installation?: object;
  installationStatus?: number;
  diff?: string;
  clock?: { value: number };
  tokenRepository?: string;
  tokenPermissions?: { issues: 'write' };
  tokenTtlMs?: number;
  tokenContentType?: string;
  tokenBodyOverride?: string;
  commitPulls?: object[];
  evidenceStatus?: number;
  evidenceResponse?: (
    url: string,
    init: RequestInit,
    fallback: () => Response,
  ) => Response | undefined;
}

function fixture(options: FixtureOptions = {}) {
  const repository = options.repository ?? 'owner/repository';
  const graph = options.graph ?? responseBody(repository);
  const confirmation = options.confirmation ?? graph;
  const clock = options.clock ?? { value: NOW };
  let graphReads = 0;
  let tokenMints = 0;

  const fetcher = vi.fn(async (input: string | URL | Request, init: RequestInit = {}) => {
    const url = String(input);
    if (url === 'https://api.github.com/app') {
      return json(options.app ?? appBody());
    }
    if (url.endsWith('/installation')) {
      if (options.installationStatus)
        return new Response(null, { status: options.installationStatus });
      return json(options.installation ?? installationBody());
    }
    if (url.includes('/access_tokens')) {
      tokenMints += 1;
      if (options.tokenBodyOverride !== undefined) {
        return new Response(options.tokenBodyOverride, {
          status: 201,
          headers: { 'content-type': options.tokenContentType ?? 'application/json' },
        });
      }
      const permissions = {
        checks: 'read',
        contents: 'read',
        issues: options.tokenPermissions?.issues ?? 'read',
        metadata: 'read',
        pull_requests: 'read',
        statuses: 'read',
      };
      return json(
        {
          token: `installation-token-${tokenMints}-${'x'.repeat(24)}`,
          expires_at: new Date(clock.value + (options.tokenTtlMs ?? 60 * 60_000)).toISOString(),
          permissions,
          repository_selection: 'selected',
          repositories: [repositoryRecord(options.tokenRepository ?? repository)],
        },
        201,
        options.tokenContentType,
      );
    }
    if (url === 'https://api.github.com/user') {
      return json(options.user ?? { id: 123, login: 'Contributor' });
    }

    const fallback = () => {
      if (options.evidenceStatus) return new Response(null, { status: options.evidenceStatus });
      if (/\/commits\/[a-f0-9]{40,64}\/pulls$/.test(url)) {
        return json(options.commitPulls ?? []);
      }
      if (url.endsWith('/pulls/23/files?per_page=100&page=1')) {
        return json([
          {
            filename: 'src/parser.ts',
            status: 'modified',
            additions: 1,
            deletions: 0,
            changes: 1,
            patch: '+handle empty input',
          },
        ]);
      }
      if (url.endsWith('/pulls/23')) {
        return new Response(options.diff ?? pullDiff, {
          status: 200,
          headers: { 'content-type': 'application/vnd.github.v3.diff' },
        });
      }
      const body = graphReads++ > 0 ? confirmation : graph;
      return json(body);
    };
    return options.evidenceResponse?.(url, init, fallback) ?? fallback();
  }) as unknown as ReturnType<typeof vi.fn> & typeof fetch;

  return {
    fetcher,
    verifier: new GitHubMergeVerifier(
      { appId: APP_ID, privateKey: PRIVATE_KEY },
      fetcher,
      () => clock.value,
    ),
  };
}

function appBody(
  id = Number(APP_ID),
  permissionOverride: Partial<Record<string, 'read' | 'write'>> = {},
) {
  return {
    id,
    slug: 'mizuki-policy-verifier',
    permissions: { ...readPermissions(), ...permissionOverride },
  };
}

function installationBody(permissionOverride: Partial<Record<string, 'read' | 'write'>> = {}) {
  return {
    id: 777,
    app_id: Number(APP_ID),
    repository_selection: 'selected',
    suspended_at: null,
    permissions: { ...readPermissions(), ...permissionOverride },
  };
}

function readPermissions() {
  return {
    checks: 'read' as const,
    contents: 'read' as const,
    issues: 'read' as const,
    metadata: 'read' as const,
    pull_requests: 'read' as const,
    statuses: 'read' as const,
  };
}

function repositoryRecord(repository: string) {
  return {
    id: 123,
    name: repository.split('/')[1],
    full_name: repository,
  };
}

function json(body: object, status = 200, contentType = 'application/json') {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': contentType },
  });
}

function mintCalls(fetcher: ReturnType<typeof vi.fn>) {
  return fetcher.mock.calls.filter(([input]) => String(input).includes('/access_tokens'));
}

function responseBody(repository = 'owner/repository') {
  return {
    data: {
      repository: {
        nameWithOwner: repository,
        visibility: 'PUBLIC' as 'PUBLIC' | 'PRIVATE' | 'INTERNAL',
        issue: {
          number: 17,
          title: 'Handle empty input',
          body: 'The parser should accept an empty input.',
        },
        pullRequest: {
          number: 23,
          state: 'MERGED' as 'OPEN' | 'CLOSED' | 'MERGED',
          merged: true,
          mergedAt: '2026-08-22T12:05:00.000Z' as string | null,
          createdAt: '2026-08-22T12:01:00.000Z',
          mergeCommit: {
            oid: 'a'.repeat(40),
            parents: {
              nodes: [{ oid: 'd'.repeat(40) }, { oid: 'b'.repeat(40) }],
            },
          } as { oid: string; parents: { nodes: { oid: string }[] } } | null,
          headRefOid: 'b'.repeat(40),
          baseRefOid: 'd'.repeat(40),
          baseRefName: 'main',
          changedFiles: 1,
          author: { login: 'contributor' },
          baseRepository: { nameWithOwner: repository },
          closingIssuesReferences: {
            nodes: [{ number: 17, repository: { nameWithOwner: repository } }],
          },
          reviews: {
            nodes: [
              {
                state: 'APPROVED',
                submittedAt: '2026-08-22T12:04:00.000Z',
                authorAssociation: 'MEMBER',
                author: { login: 'maintainer' },
                commit: { oid: 'b'.repeat(40) },
              },
            ],
          },
          commits: {
            nodes: [
              {
                commit: {
                  oid: 'b'.repeat(40),
                  statusCheckRollup: { state: 'SUCCESS', contexts: { totalCount: 2 } },
                },
              },
            ],
          },
        },
      },
    },
  };
}
