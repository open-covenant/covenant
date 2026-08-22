import { z } from 'zod';
import { PolicyError } from './domain.js';

const GITHUB_GRAPHQL_URL = 'https://api.github.com/graphql';
const GITHUB_USER_URL = 'https://api.github.com/user';
const GITHUB_API_VERSION = '2022-11-28';

export interface MergeVerificationRequest {
  repository: string;
  issueNumber: number;
  claimantGitHubLogin: string;
  pullRequestNumber: number;
  authorizedAt: Date;
}

export interface MergeEvidence {
  repository: string;
  issueNumber: number;
  claimantGitHubLogin: string;
  pullRequestNumber: number;
  pullRequestUrl: string;
  mergeCommitOid: string;
  createdAt: string;
  mergedAt: string;
}

export interface MergeVerifier {
  verify(request: MergeVerificationRequest): Promise<MergeEvidence>;
  verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence>;
  verifyOauthIdentity(accessToken: string): Promise<ClaimantEvidence>;
}

export interface RepositoryMergeRequest {
  repository: string;
  pullRequestNumber: number;
}

export interface RepositoryMergeEvidence {
  repository: string;
  pullRequestNumber: number;
  pullRequestUrl: string;
  mergeCommitOid: string;
  createdAt: string;
  mergedAt: string;
}

export interface ClaimantEvidence {
  login: string;
  githubId: string;
}

const responseSchema = z
  .object({
    data: z
      .object({
        repository: z
          .object({
            visibility: z.enum(['PUBLIC', 'PRIVATE', 'INTERNAL']),
            pullRequest: z
              .object({
                number: z.number().int().positive(),
                state: z.enum(['OPEN', 'CLOSED', 'MERGED']),
                merged: z.boolean(),
                mergedAt: z.string().datetime({ offset: true }).nullable(),
                createdAt: z.string().datetime({ offset: true }),
                mergeCommit: z.object({ oid: z.string().regex(/^[a-f0-9]{40,64}$/) }).nullable(),
                author: z.object({ login: z.string() }).nullable(),
                baseRepository: z.object({ nameWithOwner: z.string() }).nullable(),
                closingIssuesReferences: z.object({
                  nodes: z.array(
                    z.object({
                      number: z.number().int().positive(),
                      repository: z.object({ nameWithOwner: z.string() }).nullable(),
                    }),
                  ),
                }),
              })
              .nullable(),
          })
          .nullable(),
      })
      .nullable(),
    errors: z.array(z.object({ message: z.string() }).passthrough()).optional(),
  })
  .passthrough();

const query = `
query MizukiMergeEvidence($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    visibility
    pullRequest(number: $number) {
      number
      state
      merged
      mergedAt
      createdAt
      mergeCommit { oid }
      author { login }
      baseRepository { nameWithOwner }
      closingIssuesReferences(first: 100) {
        nodes {
          number
          repository { nameWithOwner }
        }
      }
    }
  }
}`;

const oauthIdentitySchema = z
  .object({ id: z.number().int().positive(), login: z.string().min(1).max(39) })
  .passthrough();

export class GitHubMergeVerifier implements MergeVerifier {
  constructor(
    private readonly token: string,
    private readonly fetcher: typeof fetch = fetch,
  ) {}

  async verifyOauthIdentity(accessToken: string): Promise<ClaimantEvidence> {
    let response: Response;
    try {
      response = await this.fetcher(GITHUB_USER_URL, {
        redirect: 'error',
        headers: {
          accept: 'application/vnd.github+json',
          authorization: `Bearer ${accessToken}`,
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
        signal: AbortSignal.timeout(7_500),
      });
    } catch {
      throw new PolicyError(
        'github_unavailable',
        'GitHub identity evidence is unavailable',
        503,
        true,
      );
    }
    if (!response.ok) {
      throw new PolicyError(
        'github_identity_invalid',
        'GitHub OAuth identity could not be verified',
        422,
      );
    }
    if (response.headers.get('content-type')?.split(';')[0]?.trim() !== 'application/json') {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned an invalid content type',
        503,
        true,
      );
    }
    const identity = oauthIdentitySchema.safeParse(await readLimitedJson(response, 8_192));
    if (!identity.success) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid identity evidence',
        503,
        true,
      );
    }
    return { githubId: String(identity.data.id), login: identity.data.login.toLowerCase() };
  }

  async verify(request: MergeVerificationRequest): Promise<MergeEvidence> {
    const [owner, name] = request.repository.split('/');
    const body = await this.graphql({
      query,
      variables: { owner, name, number: request.pullRequestNumber },
    });
    const decoded = responseSchema.safeParse(body);
    if (!decoded.success || decoded.data.errors?.length) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid merge evidence',
        503,
        true,
      );
    }
    const repositoryRecord = decoded.data.data?.repository;
    const pullRequest = repositoryRecord?.pullRequest;
    if (!pullRequest) {
      throw new PolicyError('github_pr_not_found', 'Pull request was not found', 422);
    }
    if (pullRequest.number !== request.pullRequestNumber) {
      throw new PolicyError(
        'github_pr_mismatch',
        'GitHub returned a different pull request',
        503,
        true,
      );
    }
    if (repositoryRecord.visibility !== 'PUBLIC') {
      throw new PolicyError('github_repository_not_public', 'Escrow repository is not public', 422);
    }

    const repository = pullRequest.baseRepository?.nameWithOwner.toLowerCase();
    if (repository !== request.repository.toLowerCase()) {
      throw new PolicyError(
        'github_repository_mismatch',
        'Pull request targets a different repository',
        422,
      );
    }
    if (
      !pullRequest.merged ||
      pullRequest.state !== 'MERGED' ||
      !pullRequest.mergedAt ||
      !pullRequest.mergeCommit
    ) {
      throw new PolicyError('github_pr_not_merged', 'Pull request is not merged', 422);
    }
    if (pullRequest.author?.login.toLowerCase() !== request.claimantGitHubLogin.toLowerCase()) {
      throw new PolicyError(
        'github_claimant_mismatch',
        'Pull request author does not match claimant',
        422,
      );
    }
    if (new Date(pullRequest.createdAt).getTime() < request.authorizedAt.getTime()) {
      throw new PolicyError('github_pr_too_old', 'Pull request predates escrow authorization', 422);
    }
    const closesIssue = pullRequest.closingIssuesReferences.nodes.some(
      (issue) =>
        issue.number === request.issueNumber &&
        issue.repository?.nameWithOwner.toLowerCase() === request.repository.toLowerCase(),
    );
    if (!closesIssue) {
      throw new PolicyError(
        'github_issue_mismatch',
        'Pull request does not close the authorized issue',
        422,
      );
    }

    return {
      repository: request.repository.toLowerCase(),
      issueNumber: request.issueNumber,
      claimantGitHubLogin: request.claimantGitHubLogin.toLowerCase(),
      pullRequestNumber: pullRequest.number,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${pullRequest.number}`,
      mergeCommitOid: pullRequest.mergeCommit.oid,
      createdAt: pullRequest.createdAt,
      mergedAt: pullRequest.mergedAt,
    };
  }

  async verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence> {
    const [owner, name] = request.repository.split('/');
    const body = await this.graphql({
      query,
      variables: { owner, name, number: request.pullRequestNumber },
    });
    const decoded = responseSchema.safeParse(body);
    if (!decoded.success || decoded.data.errors?.length) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid merge evidence',
        503,
        true,
      );
    }
    const repositoryRecord = decoded.data.data?.repository;
    const pullRequest = repositoryRecord?.pullRequest;
    if (!pullRequest) {
      throw new PolicyError('github_pr_not_found', 'Pull request was not found', 422);
    }
    if (pullRequest.number !== request.pullRequestNumber) {
      throw new PolicyError(
        'github_pr_mismatch',
        'GitHub returned a different pull request',
        503,
        true,
      );
    }
    if (repositoryRecord.visibility !== 'PUBLIC') {
      throw new PolicyError('github_repository_not_public', 'Repository is not public', 422);
    }
    if (
      pullRequest.baseRepository?.nameWithOwner.toLowerCase() !== request.repository.toLowerCase()
    ) {
      throw new PolicyError(
        'github_repository_mismatch',
        'Pull request targets a different repository',
        422,
      );
    }
    if (
      !pullRequest.merged ||
      pullRequest.state !== 'MERGED' ||
      !pullRequest.mergedAt ||
      !pullRequest.mergeCommit
    ) {
      throw new PolicyError('github_pr_not_merged', 'Pull request is not merged', 422);
    }
    return {
      repository: request.repository.toLowerCase(),
      pullRequestNumber: pullRequest.number,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${pullRequest.number}`,
      mergeCommitOid: pullRequest.mergeCommit.oid,
      createdAt: pullRequest.createdAt,
      mergedAt: pullRequest.mergedAt,
    };
  }

  private async graphql(body: Record<string, unknown>): Promise<unknown> {
    let response: Response;
    try {
      response = await this.fetcher(GITHUB_GRAPHQL_URL, {
        method: 'POST',
        redirect: 'error',
        headers: {
          accept: 'application/vnd.github+json',
          authorization: `Bearer ${this.token}`,
          'content-type': 'application/json',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(7_500),
      });
    } catch {
      throw new PolicyError(
        'github_unavailable',
        'GitHub merge evidence is unavailable',
        503,
        true,
      );
    }
    if (!response.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub merge evidence is unavailable',
        503,
        true,
      );
    }
    if (response.headers.get('content-type')?.split(';')[0]?.trim() !== 'application/json') {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned an invalid content type',
        503,
        true,
      );
    }

    return readLimitedJson(response, 65_536);
  }
}

export class MockMergeVerifier implements MergeVerifier {
  readonly requests: MergeVerificationRequest[] = [];
  readonly repositoryRequests: RepositoryMergeRequest[] = [];
  error: Error | null = null;
  oauthIdentity: ClaimantEvidence = { githubId: '42', login: 'contributor' };

  async verifyOauthIdentity(_accessToken: string): Promise<ClaimantEvidence> {
    if (this.error) throw this.error;
    return { ...this.oauthIdentity };
  }

  async verify(request: MergeVerificationRequest): Promise<MergeEvidence> {
    this.requests.push(structuredClone(request));
    if (this.error) throw this.error;
    return {
      repository: request.repository.toLowerCase(),
      issueNumber: request.issueNumber,
      claimantGitHubLogin: request.claimantGitHubLogin.toLowerCase(),
      pullRequestNumber: request.pullRequestNumber,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
      mergeCommitOid: 'a'.repeat(40),
      createdAt: new Date(request.authorizedAt.getTime() + 1_000).toISOString(),
      mergedAt: new Date(request.authorizedAt.getTime() + 2_000).toISOString(),
    };
  }

  async verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence> {
    this.repositoryRequests.push(structuredClone(request));
    if (this.error) throw this.error;
    const now = new Date();
    return {
      repository: request.repository.toLowerCase(),
      pullRequestNumber: request.pullRequestNumber,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
      mergeCommitOid: 'a'.repeat(40),
      createdAt: now.toISOString(),
      mergedAt: new Date(now.getTime() + 1_000).toISOString(),
    };
  }
}

async function readLimitedJson(response: Response, limit: number): Promise<unknown> {
  if (!response.body) {
    throw new PolicyError('github_invalid_response', 'GitHub response body is empty', 503, true);
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      throw new PolicyError('github_invalid_response', 'GitHub response is too large', 503, true);
    }
    chunks.push(value);
  }
  try {
    return JSON.parse(Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString('utf8'));
  } catch {
    throw new PolicyError(
      'github_invalid_response',
      'GitHub response is not valid JSON',
      503,
      true,
    );
  }
}
