import { createHash, createPrivateKey, sign, type KeyObject } from 'node:crypto';
import { z } from 'zod';
import { PolicyError } from './domain.js';

const GITHUB_API_URL = 'https://api.github.com';
const GITHUB_GRAPHQL_URL = `${GITHUB_API_URL}/graphql`;
const GITHUB_USER_URL = `${GITHUB_API_URL}/user`;
const GITHUB_API_VERSION = '2022-11-28';
const REQUEST_TIMEOUT_MS = 7_500;
const APP_JWT_CLOCK_SKEW_SECONDS = 60;
const APP_JWT_LIFETIME_SECONDS = 540;
const TOKEN_REFRESH_WINDOW_MS = 5 * 60_000;
const TOKEN_MAX_TTL_MS = 65 * 60_000;
const TOKEN_CACHE_LIMIT = 256;
const JSON_LIMIT = 65_536;
const DIFF_LIMIT = 2_000_000;
const COMPARISON_LIMIT = 2_000_000;
const DELIVERY_DIFF_DOMAIN = 'mizuki.delivery-diff.v1\0';
const DIFF_INDEX_OBJECTS = /^index ([0-9a-f]{4,64})\.\.([0-9a-f]{4,64})( [0-7]{6})?$/gm;
const INSTALLATION_PERMISSIONS = {
  checks: 'read',
  contents: 'read',
  issues: 'read',
  metadata: 'read',
  pull_requests: 'read',
  statuses: 'read',
} as const;
const forbiddenPath =
  /(^|\/)(\.github\/workflows|\.env|secrets?|vendor|generated|dist|build|node_modules)(\/|$)|(^|\/)(package-lock\.json|pnpm-lock\.yaml|yarn\.lock)$/i;

export interface GitHubAppConfig {
  appId: string;
  privateKey: string;
}

export interface MergeVerificationRequest {
  repository: string;
  issueNumber: number;
  claimantGitHubLogin: string;
  pullRequestNumber: number;
  mergeCommitSha: string;
  reviewedHeadSha: string;
  reviewedBaseSha: string;
  reviewedBaseRef: string;
  reviewedDiffHash: string;
  expectedIssueTitle: string;
  expectedIssueBody: string;
  maxFiles: number;
  authorizedAt: Date;
}

export interface MergeEvidence {
  repository: string;
  issueNumber: number;
  claimantGitHubLogin: string;
  pullRequestNumber: number;
  pullRequestUrl: string;
  mergeCommitOid: string;
  headCommitOid: string;
  baseCommitOid: string;
  baseRefName: string;
  diffHash: string;
  approvedReviewer: string;
  approvedReviewSubmittedAt: string;
  checkCount: number;
  createdAt: string;
  mergedAt: string;
}

export interface MergeReviewArtifact {
  issueTitle: string;
  issueBody: string;
  changedFiles: number;
  diff: string;
}

export interface VerifiedMerge {
  evidence: MergeEvidence;
  artifact: MergeReviewArtifact;
}

export interface MergeVerifier {
  health(): Promise<void>;
  repositoryReadiness(repository: string): Promise<RepositoryReadinessEvidence>;
  verify(request: MergeVerificationRequest): Promise<VerifiedMerge>;
  assertCommitUnpublished(repository: string, commitSha: string): Promise<void>;
  verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence>;
  verifyOauthIdentity(accessToken: string): Promise<ClaimantEvidence>;
  verifyEscrowTerms(request: EscrowTermsRequest): Promise<EscrowTermsEvidence>;
}

export interface EscrowTermsRequest {
  repository: string;
  issueNumber: number;
  issueTitle: string;
  issueBody: string;
  baseRef: string;
  baseSha: string;
}

export type EscrowTermsEvidence = EscrowTermsRequest & {
  visibility: 'PUBLIC';
};

export interface RepositoryMergeRequest {
  repository: string;
  issueNumber: number;
  pullRequestNumber: number;
  deliveredCommitSha: string;
  reviewedHeadSha: string;
  reviewedBaseSha: string;
  reviewedBaseRef: string;
  reviewedDiffHash: string;
  notBefore: Date;
}

export interface RepositoryMergeEvidence {
  repository: string;
  issueNumber: number;
  pullRequestNumber: number;
  pullRequestUrl: string;
  mergeCommitOid: string;
  headCommitOid: string;
  baseCommitOid: string;
  baseRefName: string;
  diffHash: string;
  createdAt: string;
  mergedAt: string;
}

export interface ClaimantEvidence {
  login: string;
  githubId: string;
}

export interface RepositoryReadinessEvidence {
  ready: true;
  repository: string;
  verifierAppId: string;
  installationId: number;
  repositorySelection: 'selected';
  permissions: typeof INSTALLATION_PERMISSIONS;
  tokenRepositories: 1;
  tokenExpiresAt: string;
}

interface CachedInstallationToken {
  token: string;
  expiresAt: number;
  installationId: number;
}

const responseSchema = z
  .object({
    data: z
      .object({
        repository: z
          .object({
            visibility: z.enum(['PUBLIC', 'PRIVATE', 'INTERNAL']),
            issue: z
              .object({
                number: z.number().int().positive(),
                title: z.string().min(1).max(512),
                body: z.string().max(100_000),
              })
              .nullable(),
            pullRequest: z
              .object({
                number: z.number().int().positive(),
                state: z.enum(['OPEN', 'CLOSED', 'MERGED']),
                merged: z.boolean(),
                mergedAt: z.string().datetime({ offset: true }).nullable(),
                createdAt: z.string().datetime({ offset: true }),
                mergeCommit: z
                  .object({
                    oid: z.string().regex(/^[a-f0-9]{40,64}$/),
                    parents: z.object({
                      nodes: z
                        .array(z.object({ oid: z.string().regex(/^[a-f0-9]{40,64}$/) }))
                        .min(1)
                        .max(3),
                    }),
                  })
                  .nullable(),
                headRefOid: z.string().regex(/^[a-f0-9]{40,64}$/),
                baseRefOid: z.string().regex(/^[a-f0-9]{40,64}$/),
                baseRefName: z.string().min(1).max(255),
                changedFiles: z.number().int().nonnegative().max(100_000),
                reviews: z.object({
                  nodes: z.array(
                    z.object({
                      state: z.enum([
                        'PENDING',
                        'COMMENTED',
                        'APPROVED',
                        'CHANGES_REQUESTED',
                        'DISMISSED',
                      ]),
                      submittedAt: z.string().datetime({ offset: true }).nullable(),
                      authorAssociation: z.enum([
                        'COLLABORATOR',
                        'CONTRIBUTOR',
                        'FIRST_TIMER',
                        'FIRST_TIME_CONTRIBUTOR',
                        'MANNEQUIN',
                        'MEMBER',
                        'NONE',
                        'OWNER',
                      ]),
                      author: z.object({ login: z.string().min(1).max(100) }).nullable(),
                      commit: z.object({ oid: z.string().regex(/^[a-f0-9]{40,64}$/) }).nullable(),
                    }),
                  ),
                }),
                commits: z.object({
                  nodes: z.array(
                    z.object({
                      commit: z.object({
                        oid: z.string().regex(/^[a-f0-9]{40,64}$/),
                        statusCheckRollup: z
                          .object({
                            state: z.enum(['ERROR', 'EXPECTED', 'FAILURE', 'PENDING', 'SUCCESS']),
                            contexts: z.object({ totalCount: z.number().int().nonnegative() }),
                          })
                          .nullable(),
                      }),
                    }),
                  ),
                }),
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
query MizukiMergeEvidence(
  $owner: String!
  $name: String!
  $pullRequestNumber: Int!
  $issueNumber: Int!
) {
  repository(owner: $owner, name: $name) {
    visibility
    issue(number: $issueNumber) {
      number
      title
      body
    }
    pullRequest(number: $pullRequestNumber) {
      number
      state
      merged
      mergedAt
      createdAt
      mergeCommit {
        oid
        parents(first: 3) { nodes { oid } }
      }
      headRefOid
      baseRefOid
      baseRefName
      changedFiles
      reviews(last: 100) {
        nodes {
          state
          submittedAt
          authorAssociation
          author { login }
          commit { oid }
        }
      }
      commits(last: 1) {
        nodes {
          commit {
            oid
            statusCheckRollup {
              state
              contexts(first: 100) { totalCount }
            }
          }
        }
      }
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

const escrowTermsQuery = `
query MizukiEscrowTerms($owner: String!, $name: String!, $issueNumber: Int!) {
  repository(owner: $owner, name: $name) {
    nameWithOwner
    visibility
    defaultBranchRef {
      name
      target { oid }
    }
    issue(number: $issueNumber) {
      number
      title
      body
    }
  }
}`;

const escrowTermsResponseSchema = z
  .object({
    data: z
      .object({
        repository: z
          .object({
            nameWithOwner: z.string().min(3).max(201),
            visibility: z.enum(['PUBLIC', 'PRIVATE', 'INTERNAL']),
            defaultBranchRef: z
              .object({
                name: z.string().min(1).max(255),
                target: z.object({ oid: z.string().regex(/^[a-f0-9]{40,64}$/) }),
              })
              .nullable(),
            issue: z
              .object({
                number: z.number().int().positive(),
                title: z.string().min(1).max(512),
                body: z.string().max(100_000),
              })
              .nullable(),
          })
          .nullable(),
      })
      .nullable(),
    errors: z.array(z.object({ message: z.string() }).passthrough()).optional(),
  })
  .passthrough();

const comparisonSchema = z
  .object({
    status: z.enum(['ahead', 'behind', 'diverged', 'identical']),
    base_commit: z.object({ sha: z.string().regex(/^[a-f0-9]{40,64}$/) }).passthrough(),
    merge_base_commit: z.object({ sha: z.string().regex(/^[a-f0-9]{40,64}$/) }).passthrough(),
  })
  .passthrough();

const oauthIdentitySchema = z
  .object({ id: z.number().int().positive(), login: z.string().min(1).max(39) })
  .passthrough();
const permissionSchema = z.record(z.string(), z.enum(['read', 'write']));
const appSchema = z
  .object({
    id: z.number().int().positive().safe(),
    slug: z.string().min(1).max(100),
    permissions: permissionSchema,
  })
  .passthrough();
const installationSchema = z
  .object({
    id: z.number().int().positive().safe(),
    app_id: z.number().int().positive().safe(),
    repository_selection: z.literal('selected'),
    permissions: permissionSchema,
    suspended_at: z.string().datetime({ offset: true }).nullable(),
  })
  .passthrough();
const installationTokenSchema = z
  .object({
    token: z.string().min(20).max(4_096).regex(/^\S+$/),
    expires_at: z.string().datetime({ offset: true }),
    permissions: z
      .object({
        checks: z.literal('read'),
        contents: z.literal('read'),
        issues: z.literal('read'),
        metadata: z.literal('read'),
        pull_requests: z.literal('read'),
        statuses: z.literal('read'),
      })
      .strict(),
    repository_selection: z.literal('selected'),
    repositories: z
      .array(
        z
          .object({
            name: z.string().min(1).max(100),
            full_name: z.string().min(3).max(201),
          })
          .passthrough(),
      )
      .length(1),
  })
  .passthrough();
const commitPullRequestsSchema = z.array(
  z
    .object({
      number: z.number().int().positive().max(2_147_483_647),
      html_url: z.string().url().max(2_048),
    })
    .passthrough(),
);
const pullRequestFilesSchema = z.array(
  z
    .object({
      filename: z.string().min(1).max(4_096),
      previous_filename: z.string().min(1).max(4_096).optional(),
      status: z.enum(['added', 'removed', 'modified', 'renamed', 'copied', 'changed', 'unchanged']),
      patch: z.string().max(DIFF_LIMIT).optional(),
    })
    .passthrough(),
);

export class GitHubMergeVerifier implements MergeVerifier {
  private readonly appId: string;
  private readonly privateKey: KeyObject;
  private readonly tokenCache = new Map<string, CachedInstallationToken>();
  private readonly tokenRequests = new Map<string, Promise<CachedInstallationToken>>();

  constructor(
    config: GitHubAppConfig,
    private readonly fetcher: typeof fetch = fetch,
    private readonly now: () => number = Date.now,
  ) {
    if (!/^[1-9]\d{0,15}$/.test(config.appId) || !Number.isSafeInteger(Number(config.appId))) {
      throw new Error('GitHub App ID must be a positive safe integer');
    }
    this.appId = config.appId;
    this.privateKey = createPrivateKey(config.privateKey);
    if (this.privateKey.type !== 'private' || this.privateKey.asymmetricKeyType !== 'rsa') {
      throw new Error('GitHub App private key must be RSA');
    }
  }

  async health(): Promise<void> {
    const response = await this.appRequest('/app');
    if (response.status === 401 || response.status === 403) {
      throw new PolicyError('github_credential_invalid', 'GitHub App credential was rejected', 503);
    }
    if (!response.ok) {
      throw new PolicyError('github_unavailable', 'GitHub App identity is unavailable', 503, true);
    }
    const decoded = appSchema.safeParse(await readJsonResponse(response, 16_384));
    if (!decoded.success || String(decoded.data.id) !== this.appId) {
      throw new PolicyError(
        'github_credential_invalid',
        'GitHub App identity does not match signer configuration',
        503,
      );
    }
    assertReadOnlyPermissions(decoded.data.permissions, 'GitHub App');
  }

  async repositoryReadiness(repository: string): Promise<RepositoryReadinessEvidence> {
    const normalized = repository.toLowerCase();
    repositoryParts(normalized);
    await this.health();

    this.tokenCache.delete(normalized);
    const token = await this.installationToken(normalized);
    return {
      ready: true,
      repository: normalized,
      verifierAppId: this.appId,
      installationId: token.installationId,
      repositorySelection: 'selected',
      permissions: INSTALLATION_PERMISSIONS,
      tokenRepositories: 1,
      tokenExpiresAt: new Date(token.expiresAt).toISOString(),
    };
  }

  async verifyOauthIdentity(accessToken: string): Promise<ClaimantEvidence> {
    const response = await this.request(
      GITHUB_USER_URL,
      {
        headers: {
          accept: 'application/vnd.github+json',
          authorization: `Bearer ${accessToken}`,
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
      },
      'GitHub identity evidence is unavailable',
    );
    if (!response.ok) {
      throw new PolicyError(
        'github_identity_invalid',
        'GitHub OAuth identity could not be verified',
        422,
      );
    }
    const identity = oauthIdentitySchema.safeParse(await readJsonResponse(response, 8_192));
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

  async verifyEscrowTerms(request: EscrowTermsRequest): Promise<EscrowTermsEvidence> {
    const [owner, name] = repositoryParts(request.repository);
    const decoded = escrowTermsResponseSchema.safeParse(
      await this.graphql(request.repository, {
        query: escrowTermsQuery,
        variables: { owner, name, issueNumber: request.issueNumber },
      }),
    );
    const repository = decoded.success ? decoded.data.data?.repository : undefined;
    const issue = repository?.issue;
    const base = repository?.defaultBranchRef;
    if (!decoded.success || decoded.data.errors?.length || !repository || !issue || !base) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned incomplete escrow acceptance evidence',
        503,
        true,
      );
    }
    if (
      repository.visibility !== 'PUBLIC' ||
      repository.nameWithOwner.toLowerCase() !== request.repository.toLowerCase() ||
      issue.number !== request.issueNumber ||
      issue.title !== request.issueTitle ||
      issue.body !== request.issueBody ||
      base.name !== request.baseRef
    ) {
      throw new PolicyError(
        'github_escrow_terms_mismatch',
        'Escrow terms do not match the public repository default branch and issue',
        422,
      );
    }
    if (base.target.oid !== request.baseSha) {
      await this.verifyDefaultBranchAncestor(request.repository, request.baseSha, base.target.oid);
    }
    return {
      ...request,
      repository: request.repository.toLowerCase(),
      visibility: 'PUBLIC',
    };
  }

  private async verifyDefaultBranchAncestor(
    repository: string,
    baseSha: string,
    currentSha: string,
  ): Promise<void> {
    const [owner, name] = repositoryParts(repository);
    const url = `${GITHUB_API_URL}/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/compare/${encodeURIComponent(baseSha)}...${encodeURIComponent(currentSha)}?per_page=1&page=1`;
    const response = await this.repositoryRequest(
      repository,
      url,
      {
        headers: {
          accept: 'application/vnd.github+json',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
      },
      'GitHub default branch ancestry is unavailable',
    );
    if (response.status === 404 || response.status === 422) {
      throw new PolicyError(
        'github_escrow_terms_mismatch',
        'Escrow base commit is not on the public repository default branch',
        422,
      );
    }
    if (!response.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub default branch ancestry is unavailable',
        503,
        true,
      );
    }
    const comparison = comparisonSchema.safeParse(
      await readJsonResponse(response, COMPARISON_LIMIT),
    );
    if (!comparison.success) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid default branch ancestry evidence',
        503,
        true,
      );
    }
    if (
      !['ahead', 'identical'].includes(comparison.data.status) ||
      comparison.data.base_commit.sha !== baseSha ||
      comparison.data.merge_base_commit.sha !== baseSha
    ) {
      throw new PolicyError(
        'github_escrow_terms_mismatch',
        'Escrow base commit is not an ancestor of the public repository default branch',
        422,
      );
    }
  }

  async verify(request: MergeVerificationRequest): Promise<VerifiedMerge> {
    const [owner, name] = repositoryParts(request.repository);
    const body = await this.graphql(request.repository, {
      query,
      variables: {
        owner,
        name,
        pullRequestNumber: request.pullRequestNumber,
        issueNumber: request.issueNumber,
      },
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
    const issue = repositoryRecord?.issue;
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
    if (
      !issue ||
      issue.number !== request.issueNumber ||
      issue.title !== request.expectedIssueTitle ||
      issue.body !== request.expectedIssueBody
    ) {
      throw new PolicyError(
        'github_issue_terms_mismatch',
        'GitHub issue terms do not match the accepted bounty commitment',
        422,
      );
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
    if (
      pullRequest.mergeCommit.oid !== request.mergeCommitSha ||
      pullRequest.headRefOid !== request.reviewedHeadSha ||
      pullRequest.baseRefName !== request.reviewedBaseRef
    ) {
      throw new PolicyError(
        'github_review_mismatch',
        'Merged pull request does not match the authorized reviewed revision',
        422,
      );
    }
    const baseCommitOid = verifiedMergeBase(
      pullRequest.mergeCommit,
      request.reviewedBaseSha,
      request.reviewedHeadSha,
    );
    if (pullRequest.changedFiles < 1 || pullRequest.changedFiles > request.maxFiles) {
      throw new PolicyError(
        'github_review_scope_mismatch',
        'Merged pull request exceeds the accepted file scope',
        422,
      );
    }
    const approval = approvedReview(
      pullRequest.reviews.nodes,
      pullRequest.headRefOid,
      request.claimantGitHubLogin,
    );
    if (!approval) {
      throw new PolicyError(
        'github_maintainer_approval_missing',
        'Latest pull request head lacks an active approval from a non-claimant maintainer',
        422,
      );
    }
    const checkCount = passingCheckCount(pullRequest.commits.nodes, pullRequest.headRefOid);
    if (checkCount === 0) {
      throw new PolicyError(
        'github_checks_missing',
        'Latest pull request head lacks successful repository checks',
        422,
      );
    }
    const files = await this.pullRequestFiles(request.repository, owner, name, pullRequest.number);
    if (files.length !== pullRequest.changedFiles || files.some(unsafeReviewFile)) {
      throw new PolicyError(
        'github_review_files_unsafe',
        'Pull request file evidence is incomplete, binary, truncated, or security-sensitive',
        422,
      );
    }

    const diffBytes = await this.pullRequestDiff(
      request.repository,
      owner,
      name,
      pullRequest.number,
    );
    const diffHash = createHash('sha256').update(diffBytes).digest('hex');
    let diff: string;
    try {
      diff = new TextDecoder('utf-8', { fatal: true }).decode(diffBytes);
    } catch {
      throw new PolicyError(
        'github_review_files_unsafe',
        'Pull request diff is not valid UTF-8 text',
        422,
      );
    }
    const confirmation = responseSchema.safeParse(
      await this.graphql(request.repository, {
        query,
        variables: {
          owner,
          name,
          pullRequestNumber: request.pullRequestNumber,
          issueNumber: request.issueNumber,
        },
      }),
    );
    const confirmedRepository = confirmation.success
      ? confirmation.data.data?.repository
      : undefined;
    const confirmedPull = confirmedRepository?.pullRequest;
    const confirmedIssue = confirmedRepository?.issue;
    const confirmedClosesIssue = confirmedPull?.closingIssuesReferences.nodes.some(
      (issue) =>
        issue.number === request.issueNumber &&
        issue.repository?.nameWithOwner.toLowerCase() === request.repository.toLowerCase(),
    );
    const confirmedApproval = confirmedPull
      ? approvedReview(
          confirmedPull.reviews.nodes,
          confirmedPull.headRefOid,
          request.claimantGitHubLogin,
        )
      : null;
    const confirmedCheckCount = confirmedPull
      ? passingCheckCount(confirmedPull.commits.nodes, confirmedPull.headRefOid)
      : 0;
    if (
      !confirmedPull ||
      !confirmedIssue ||
      (confirmation.success && confirmation.data.errors?.length) ||
      confirmedRepository?.visibility !== 'PUBLIC' ||
      confirmedIssue.number !== issue.number ||
      confirmedIssue.title !== issue.title ||
      confirmedIssue.body !== issue.body ||
      confirmedPull.number !== pullRequest.number ||
      confirmedPull.state !== 'MERGED' ||
      !confirmedPull.merged ||
      !confirmedClosesIssue ||
      confirmedPull.baseRepository?.nameWithOwner.toLowerCase() !==
        request.repository.toLowerCase() ||
      confirmedPull.author?.login.toLowerCase() !== request.claimantGitHubLogin.toLowerCase() ||
      confirmedPull.createdAt !== pullRequest.createdAt ||
      confirmedPull.changedFiles !== pullRequest.changedFiles ||
      confirmedApproval?.login !== approval.login ||
      confirmedApproval?.submittedAt !== approval.submittedAt ||
      confirmedApproval?.commitOid !== approval.commitOid ||
      confirmedCheckCount !== checkCount ||
      confirmedPull.headRefOid !== pullRequest.headRefOid ||
      confirmedPull.baseRefName !== pullRequest.baseRefName ||
      confirmedPull.mergedAt !== pullRequest.mergedAt ||
      confirmedPull.mergeCommit?.oid !== pullRequest.mergeCommit.oid ||
      mergeParentOids(confirmedPull.mergeCommit).join(':') !==
        mergeParentOids(pullRequest.mergeCommit).join(':')
    ) {
      throw new PolicyError(
        'github_evidence_changed',
        'GitHub merge evidence changed while the reviewed artifact was verified',
        503,
        true,
      );
    }
    if (diffHash !== request.reviewedDiffHash) {
      throw new PolicyError(
        'github_review_diff_mismatch',
        'Merged pull request diff does not match the reviewed artifact',
        422,
      );
    }
    if (/^GIT binary patch$/m.test(diff) || /^Binary files .* differ$/m.test(diff)) {
      throw new PolicyError(
        'github_review_files_unsafe',
        'Binary pull request diffs cannot authorize escrow release',
        422,
      );
    }

    return {
      evidence: {
        repository: request.repository.toLowerCase(),
        issueNumber: request.issueNumber,
        claimantGitHubLogin: request.claimantGitHubLogin.toLowerCase(),
        pullRequestNumber: pullRequest.number,
        pullRequestUrl: `https://github.com/${request.repository}/pull/${pullRequest.number}`,
        mergeCommitOid: pullRequest.mergeCommit.oid,
        headCommitOid: pullRequest.headRefOid,
        baseCommitOid,
        baseRefName: pullRequest.baseRefName,
        diffHash,
        approvedReviewer: approval.login,
        approvedReviewSubmittedAt: approval.submittedAt,
        checkCount,
        createdAt: pullRequest.createdAt,
        mergedAt: pullRequest.mergedAt,
      },
      artifact: {
        issueTitle: issue.title,
        issueBody: issue.body,
        changedFiles: pullRequest.changedFiles,
        diff,
      },
    };
  }

  async verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence> {
    const [owner, name] = repositoryParts(request.repository);
    const body = await this.graphql(request.repository, {
      query,
      variables: {
        owner,
        name,
        pullRequestNumber: request.pullRequestNumber,
        issueNumber: request.issueNumber,
      },
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
    const notBefore = request.notBefore.getTime();
    if (
      new Date(pullRequest.createdAt).getTime() < notBefore ||
      new Date(pullRequest.mergedAt).getTime() < notBefore
    ) {
      throw new PolicyError(
        'github_pr_too_old',
        'Pull request creation and merge must follow payment authorization',
        422,
      );
    }
    const closesIssue = pullRequest.closingIssuesReferences.nodes.some(
      (issue) =>
        issue.number === request.issueNumber &&
        issue.repository?.nameWithOwner.toLowerCase() === request.repository.toLowerCase(),
    );
    if (!closesIssue) {
      throw new PolicyError(
        'github_issue_mismatch',
        'Pull request does not close the registered issue',
        422,
      );
    }
    if (
      pullRequest.headRefOid !== request.deliveredCommitSha ||
      pullRequest.headRefOid !== request.reviewedHeadSha ||
      pullRequest.baseRefName !== request.reviewedBaseRef
    ) {
      throw new PolicyError(
        'github_review_mismatch',
        'Merged pull request does not match the registered delivery evidence',
        422,
      );
    }
    const baseCommitOid = verifiedMergeBase(
      pullRequest.mergeCommit,
      request.reviewedBaseSha,
      request.reviewedHeadSha,
    );

    const diffBytes = await this.pullRequestDiff(
      request.repository,
      owner,
      name,
      pullRequest.number,
    );
    let diff: string;
    try {
      diff = new TextDecoder('utf-8', { fatal: true }).decode(diffBytes);
    } catch {
      throw new PolicyError(
        'github_review_files_unsafe',
        'Pull request diff is not valid UTF-8 text',
        422,
      );
    }
    const diffHash = deliveryDiffHash(diff);
    const confirmation = responseSchema.safeParse(
      await this.graphql(request.repository, {
        query,
        variables: {
          owner,
          name,
          pullRequestNumber: request.pullRequestNumber,
          issueNumber: request.issueNumber,
        },
      }),
    );
    const confirmedRepository = confirmation.success
      ? confirmation.data.data?.repository
      : undefined;
    const confirmedPull = confirmedRepository?.pullRequest;
    const confirmedClosesIssue = confirmedPull?.closingIssuesReferences.nodes.some(
      (issue) =>
        issue.number === request.issueNumber &&
        issue.repository?.nameWithOwner.toLowerCase() === request.repository.toLowerCase(),
    );
    if (
      !confirmedPull ||
      (confirmation.success && confirmation.data.errors?.length) ||
      confirmedRepository?.visibility !== 'PUBLIC' ||
      confirmedPull.number !== pullRequest.number ||
      confirmedPull.state !== 'MERGED' ||
      !confirmedPull.merged ||
      !confirmedClosesIssue ||
      confirmedPull.baseRepository?.nameWithOwner.toLowerCase() !==
        request.repository.toLowerCase() ||
      confirmedPull.createdAt !== pullRequest.createdAt ||
      confirmedPull.headRefOid !== pullRequest.headRefOid ||
      confirmedPull.baseRefName !== pullRequest.baseRefName ||
      confirmedPull.mergedAt !== pullRequest.mergedAt ||
      confirmedPull.mergeCommit?.oid !== pullRequest.mergeCommit.oid ||
      mergeParentOids(confirmedPull.mergeCommit).join(':') !==
        mergeParentOids(pullRequest.mergeCommit).join(':')
    ) {
      throw new PolicyError(
        'github_evidence_changed',
        'GitHub merge evidence changed while the delivered artifact was verified',
        503,
        true,
      );
    }
    if (diffHash !== request.reviewedDiffHash) {
      throw new PolicyError(
        'github_review_diff_mismatch',
        'Merged pull request diff does not match the reviewed delivery artifact',
        422,
      );
    }
    return {
      repository: request.repository.toLowerCase(),
      issueNumber: request.issueNumber,
      pullRequestNumber: pullRequest.number,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${pullRequest.number}`,
      mergeCommitOid: pullRequest.mergeCommit.oid,
      headCommitOid: pullRequest.headRefOid,
      baseCommitOid,
      baseRefName: pullRequest.baseRefName,
      diffHash,
      createdAt: pullRequest.createdAt,
      mergedAt: pullRequest.mergedAt,
    };
  }

  async assertCommitUnpublished(repository: string, commitSha: string): Promise<void> {
    const [owner, name] = repositoryParts(repository);
    const response = await this.repositoryRequest(
      repository,
      `${GITHUB_API_URL}/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/commits/${encodeURIComponent(commitSha)}/pulls`,
      {
        headers: {
          accept: 'application/vnd.github+json',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
      },
      'GitHub commit publication evidence is unavailable',
    );
    if (!response.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub commit publication evidence is unavailable',
        503,
        true,
      );
    }
    const pulls = commitPullRequestsSchema.safeParse(await readJsonResponse(response, JSON_LIMIT));
    if (!pulls.success) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid commit publication evidence',
        503,
        true,
      );
    }
    if (pulls.data.length > 0) {
      throw new PolicyError(
        'github_delivery_already_published',
        'Reviewed delivery was published before its liability binding was recorded',
        422,
      );
    }
  }

  private async pullRequestFiles(
    repository: string,
    owner: string,
    name: string,
    number: number,
  ): Promise<z.infer<typeof pullRequestFilesSchema>> {
    const response = await this.repositoryRequest(
      repository,
      `${GITHUB_API_URL}/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/pulls/${number}/files?per_page=100&page=1`,
      {
        headers: {
          accept: 'application/vnd.github+json',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
      },
      'GitHub pull request file evidence is unavailable',
    );
    if (!response.ok) {
      throw new PolicyError(
        'github_files_unavailable',
        'GitHub pull request file evidence could not be retrieved',
        503,
        true,
      );
    }
    const decoded = pullRequestFilesSchema.safeParse(await readJsonResponse(response, DIFF_LIMIT));
    if (!decoded.success) {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned invalid pull request file evidence',
        503,
        true,
      );
    }
    return decoded.data;
  }

  private async pullRequestDiff(
    repository: string,
    owner: string,
    name: string,
    number: number,
  ): Promise<Buffer> {
    const response = await this.repositoryRequest(
      repository,
      `${GITHUB_API_URL}/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/pulls/${number}`,
      {
        headers: {
          accept: 'application/vnd.github.v3.diff',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
      },
      'GitHub review artifact is unavailable',
    );
    if (!response.ok) {
      throw new PolicyError(
        'github_diff_unavailable',
        'GitHub review artifact could not be retrieved',
        503,
        true,
      );
    }
    const contentType = mediaType(response);
    if (contentType !== 'application/vnd.github.v3.diff' && contentType !== 'text/plain') {
      throw new PolicyError(
        'github_invalid_response',
        'GitHub returned an invalid review artifact content type',
        503,
        true,
      );
    }
    const length = response.headers.get('content-length');
    if (length && (!/^\d+$/.test(length) || Number(length) > DIFF_LIMIT)) {
      throw new PolicyError('github_diff_too_large', 'GitHub review artifact is too large', 422);
    }
    return readLimitedBody(response, DIFF_LIMIT, 'GitHub review artifact');
  }

  private async graphql(repository: string, body: Record<string, unknown>): Promise<unknown> {
    const response = await this.repositoryRequest(
      repository,
      GITHUB_GRAPHQL_URL,
      {
        method: 'POST',
        headers: {
          accept: 'application/vnd.github+json',
          'content-type': 'application/json',
          'user-agent': 'mizuki-policy-signer/0.1',
          'x-github-api-version': GITHUB_API_VERSION,
        },
        body: JSON.stringify(body),
      },
      'GitHub merge evidence is unavailable',
    );
    if (!response.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub merge evidence is unavailable',
        503,
        true,
      );
    }
    return readJsonResponse(response, JSON_LIMIT);
  }

  private async repositoryRequest(
    repository: string,
    url: string,
    init: RequestInit,
    unavailableMessage: string,
  ): Promise<Response> {
    const first = await this.installationToken(repository);
    const response = await this.request(
      url,
      withAuthorization(init, first.token),
      unavailableMessage,
    );
    if (response.status !== 401) return response;

    if (response.body) {
      try {
        await response.body.cancel();
      } catch {
        throw new PolicyError(
          'github_unavailable',
          'GitHub authentication response could not be closed',
          503,
          true,
        );
      }
    }
    this.invalidateToken(repository, first.token);
    const replacement = await this.installationToken(repository);
    return this.request(url, withAuthorization(init, replacement.token), unavailableMessage);
  }

  private async installationToken(repository: string): Promise<CachedInstallationToken> {
    const key = repository.toLowerCase();
    const cached = this.tokenCache.get(key);
    if (cached && cached.expiresAt - this.now() > TOKEN_REFRESH_WINDOW_MS) {
      this.tokenCache.delete(key);
      this.tokenCache.set(key, cached);
      return cached;
    }
    if (cached) this.tokenCache.delete(key);

    const pending = this.tokenRequests.get(key);
    if (pending) return pending;

    const request = this.mintInstallationToken(repository).finally(() => {
      if (this.tokenRequests.get(key) === request) this.tokenRequests.delete(key);
    });
    this.tokenRequests.set(key, request);
    return request;
  }

  private async mintInstallationToken(repository: string): Promise<CachedInstallationToken> {
    const [owner, name] = repositoryParts(repository);
    const installationResponse = await this.appRequest(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(name)}/installation`,
    );
    if (installationResponse.status === 404) {
      throw new PolicyError(
        'github_app_not_installed',
        'GitHub App is not installed for the requested repository',
        422,
      );
    }
    if (installationResponse.status === 401 || installationResponse.status === 403) {
      throw new PolicyError('github_credential_invalid', 'GitHub App credential was rejected', 503);
    }
    if (!installationResponse.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub App installation is unavailable',
        503,
        true,
      );
    }
    const installation = installationSchema.safeParse(
      await readJsonResponse(installationResponse, 32_768),
    );
    if (!installation.success || String(installation.data.app_id) !== this.appId) {
      throw new PolicyError(
        'github_credential_invalid',
        'GitHub returned an invalid App installation',
        503,
      );
    }
    if (installation.data.suspended_at) {
      throw new PolicyError(
        'github_app_not_installed',
        'GitHub App installation is suspended for the requested repository',
        422,
      );
    }
    assertReadOnlyPermissions(installation.data.permissions, 'GitHub App installation');

    const tokenResponse = await this.appRequest(
      `/app/installations/${installation.data.id}/access_tokens`,
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          repositories: [name],
          permissions: INSTALLATION_PERMISSIONS,
        }),
      },
    );
    if (tokenResponse.status === 401 || tokenResponse.status === 403) {
      throw new PolicyError(
        'github_credential_invalid',
        'GitHub App could not mint a repository token',
        503,
      );
    }
    if (tokenResponse.status === 404) {
      throw new PolicyError(
        'github_app_not_installed',
        'GitHub App installation disappeared before token issuance',
        422,
      );
    }
    if (!tokenResponse.ok) {
      throw new PolicyError(
        'github_unavailable',
        'GitHub repository credential is unavailable',
        503,
        true,
      );
    }
    const decoded = installationTokenSchema.safeParse(
      await readJsonResponse(tokenResponse, JSON_LIMIT),
    );
    if (
      !decoded.success ||
      decoded.data.repositories[0]?.full_name.toLowerCase() !== repository.toLowerCase()
    ) {
      throw new PolicyError(
        'github_credential_invalid',
        'GitHub returned an incorrectly scoped repository token',
        503,
      );
    }

    const expiresAt = new Date(decoded.data.expires_at).getTime();
    const ttl = expiresAt - this.now();
    if (ttl <= TOKEN_REFRESH_WINDOW_MS || ttl > TOKEN_MAX_TTL_MS) {
      throw new PolicyError(
        'github_credential_invalid',
        'GitHub returned an invalid repository token lifetime',
        503,
      );
    }
    const cached = {
      token: decoded.data.token,
      expiresAt,
      installationId: installation.data.id,
    };
    this.cacheToken(repository, cached);
    return cached;
  }

  private cacheToken(repository: string, token: CachedInstallationToken): void {
    const key = repository.toLowerCase();
    if (!this.tokenCache.has(key) && this.tokenCache.size >= TOKEN_CACHE_LIMIT) {
      const oldest = this.tokenCache.keys().next().value;
      if (oldest) this.tokenCache.delete(oldest);
    }
    this.tokenCache.delete(key);
    this.tokenCache.set(key, token);
  }

  private invalidateToken(repository: string, token: string): void {
    const key = repository.toLowerCase();
    if (this.tokenCache.get(key)?.token === token) this.tokenCache.delete(key);
  }

  private appRequest(path: string, init: RequestInit = {}): Promise<Response> {
    const jwt = createGitHubAppJwt(this.appId, this.privateKey, this.now());
    return this.request(
      `${GITHUB_API_URL}${path}`,
      withAuthorization(
        {
          ...init,
          headers: {
            accept: 'application/vnd.github+json',
            'user-agent': 'mizuki-policy-signer/0.1',
            'x-github-api-version': GITHUB_API_VERSION,
            ...init.headers,
          },
        },
        jwt,
      ),
      'GitHub App service is unavailable',
    );
  }

  private async request(
    url: string,
    init: RequestInit,
    unavailableMessage: string,
  ): Promise<Response> {
    try {
      return await this.fetcher(url, {
        ...init,
        redirect: 'error',
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch {
      throw new PolicyError('github_unavailable', unavailableMessage, 503, true);
    }
  }
}

type PullRequestReview = {
  state: 'PENDING' | 'COMMENTED' | 'APPROVED' | 'CHANGES_REQUESTED' | 'DISMISSED';
  submittedAt: string | null;
  authorAssociation:
    | 'COLLABORATOR'
    | 'CONTRIBUTOR'
    | 'FIRST_TIMER'
    | 'FIRST_TIME_CONTRIBUTOR'
    | 'MANNEQUIN'
    | 'MEMBER'
    | 'NONE'
    | 'OWNER';
  author: { login: string } | null;
  commit: { oid: string } | null;
};

type PullRequestCommit = {
  commit: {
    oid: string;
    statusCheckRollup: {
      state: 'ERROR' | 'EXPECTED' | 'FAILURE' | 'PENDING' | 'SUCCESS';
      contexts: { totalCount: number };
    } | null;
  };
};

type MergeCommit = {
  oid: string;
  parents: { nodes: { oid: string }[] };
};

function mergeParentOids(commit: MergeCommit | null | undefined): string[] {
  return commit?.parents.nodes.map(({ oid }) => oid) ?? [];
}

function verifiedMergeBase(
  commit: MergeCommit,
  reviewedBaseSha: string,
  reviewedHeadSha: string,
): string {
  const parents = mergeParentOids(commit);
  const squashOrSingleCommitRebase = parents.length === 1 && parents[0] === reviewedBaseSha;
  const mergeCommit =
    parents.length === 2 && parents[0] === reviewedBaseSha && parents[1] === reviewedHeadSha;
  if (!squashOrSingleCommitRebase && !mergeCommit) {
    throw new PolicyError(
      'github_merge_lineage_mismatch',
      'Merged commit ancestry does not bind the reviewed base and head revisions',
      422,
    );
  }
  return reviewedBaseSha;
}

function approvedReview(
  reviews: PullRequestReview[],
  headSha: string,
  claimantLogin: string,
): { login: string; submittedAt: string; commitOid: string } | null {
  const trustedAssociations = new Set(['OWNER', 'MEMBER', 'COLLABORATOR']);
  const latestByReviewer = new Map<string, PullRequestReview>();
  for (const review of reviews
    .filter(
      (review) =>
        review.submittedAt &&
        review.author &&
        review.author.login.toLowerCase() !== claimantLogin.toLowerCase() &&
        trustedAssociations.has(review.authorAssociation) &&
        review.commit?.oid === headSha,
    )
    .sort((left, right) => right.submittedAt!.localeCompare(left.submittedAt!))) {
    const login = review.author!.login.toLowerCase();
    if (!latestByReviewer.has(login)) latestByReviewer.set(login, review);
  }
  const review = [...latestByReviewer.values()]
    .filter(({ state }) => state === 'APPROVED')
    .sort((left, right) => right.submittedAt!.localeCompare(left.submittedAt!))[0];
  return review?.author && review.submittedAt && review.commit
    ? {
        login: review.author.login.toLowerCase(),
        submittedAt: review.submittedAt,
        commitOid: review.commit.oid,
      }
    : null;
}

function passingCheckCount(commits: PullRequestCommit[], headSha: string): number {
  const commit = commits.length === 1 ? commits[0]?.commit : undefined;
  const rollup = commit?.oid === headSha ? commit.statusCheckRollup : null;
  return rollup?.state === 'SUCCESS' && rollup.contexts.totalCount > 0
    ? rollup.contexts.totalCount
    : 0;
}

function unsafeReviewFile(file: z.infer<typeof pullRequestFilesSchema>[number]): boolean {
  return (
    forbiddenPath.test(file.filename) ||
    (file.previous_filename ? forbiddenPath.test(file.previous_filename) : false) ||
    file.patch === undefined
  );
}

export class MockMergeVerifier implements MergeVerifier {
  readonly requests: MergeVerificationRequest[] = [];
  readonly repositoryRequests: RepositoryMergeRequest[] = [];
  readonly readinessRequests: string[] = [];
  readonly unpublishedRequests: Array<{ repository: string; commitSha: string }> = [];
  error: Error | null = null;
  oauthIdentity: ClaimantEvidence = { githubId: '42', login: 'contributor' };

  async health(): Promise<void> {
    if (this.error) throw this.error;
  }

  async repositoryReadiness(repository: string): Promise<RepositoryReadinessEvidence> {
    this.readinessRequests.push(repository);
    if (this.error) throw this.error;
    return {
      ready: true,
      repository: repository.toLowerCase(),
      verifierAppId: '12345',
      installationId: 777,
      repositorySelection: 'selected',
      permissions: INSTALLATION_PERMISSIONS,
      tokenRepositories: 1,
      tokenExpiresAt: new Date(Date.now() + 60 * 60_000).toISOString(),
    };
  }

  async verifyOauthIdentity(_accessToken: string): Promise<ClaimantEvidence> {
    if (this.error) throw this.error;
    return { ...this.oauthIdentity };
  }

  async verifyEscrowTerms(request: EscrowTermsRequest): Promise<EscrowTermsEvidence> {
    if (this.error) throw this.error;
    return {
      ...structuredClone(request),
      repository: request.repository.toLowerCase(),
      visibility: 'PUBLIC',
    };
  }

  async verify(request: MergeVerificationRequest): Promise<VerifiedMerge> {
    this.requests.push(structuredClone(request));
    if (this.error) throw this.error;
    return {
      evidence: {
        repository: request.repository.toLowerCase(),
        issueNumber: request.issueNumber,
        claimantGitHubLogin: request.claimantGitHubLogin.toLowerCase(),
        pullRequestNumber: request.pullRequestNumber,
        pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
        mergeCommitOid: request.mergeCommitSha,
        headCommitOid: request.reviewedHeadSha,
        baseCommitOid: request.reviewedBaseSha,
        baseRefName: request.reviewedBaseRef,
        diffHash: request.reviewedDiffHash,
        approvedReviewer: 'maintainer',
        approvedReviewSubmittedAt: new Date(request.authorizedAt.getTime() + 1_500).toISOString(),
        checkCount: 1,
        createdAt: new Date(request.authorizedAt.getTime() + 1_000).toISOString(),
        mergedAt: new Date(request.authorizedAt.getTime() + 2_000).toISOString(),
      },
      artifact: {
        issueTitle: request.expectedIssueTitle,
        issueBody: request.expectedIssueBody,
        changedFiles: 1,
        diff: 'diff --git a/src/fix.ts b/src/fix.ts\n',
      },
    };
  }

  async assertCommitUnpublished(repository: string, commitSha: string): Promise<void> {
    this.unpublishedRequests.push({ repository, commitSha });
    if (this.error) throw this.error;
  }

  async verifyRepositoryMerge(request: RepositoryMergeRequest): Promise<RepositoryMergeEvidence> {
    this.repositoryRequests.push(structuredClone(request));
    if (this.error) throw this.error;
    const now = new Date();
    return {
      repository: request.repository.toLowerCase(),
      issueNumber: request.issueNumber,
      pullRequestNumber: request.pullRequestNumber,
      pullRequestUrl: `https://github.com/${request.repository}/pull/${request.pullRequestNumber}`,
      mergeCommitOid: 'a'.repeat(40),
      headCommitOid: request.deliveredCommitSha,
      baseCommitOid: request.reviewedBaseSha,
      baseRefName: request.reviewedBaseRef,
      diffHash: request.reviewedDiffHash,
      createdAt: now.toISOString(),
      mergedAt: new Date(now.getTime() + 1_000).toISOString(),
    };
  }
}

export function createGitHubAppJwt(appId: string, privateKey: KeyObject, nowMs: number): string {
  const nowSeconds = Math.floor(nowMs / 1_000);
  const header = base64UrlJson({ alg: 'RS256', typ: 'JWT' });
  const payload = base64UrlJson({
    iat: nowSeconds - APP_JWT_CLOCK_SKEW_SECONDS,
    exp: nowSeconds + APP_JWT_LIFETIME_SECONDS,
    iss: appId,
  });
  const message = `${header}.${payload}`;
  const signature = sign('RSA-SHA256', Buffer.from(message), privateKey).toString('base64url');
  return `${message}.${signature}`;
}

export function deliveryDiffHash(diff: string): string {
  return createHash('sha256')
    .update(DELIVERY_DIFF_DOMAIN)
    .update(
      diff.replace(DIFF_INDEX_OBJECTS, (_line, oldObject, newObject, mode = '') => {
        const oldKind = /^0+$/.test(oldObject) ? '<zero>' : '<object>';
        const newKind = /^0+$/.test(newObject) ? '<zero>' : '<object>';
        return `index ${oldKind}..${newKind}${mode}`;
      }),
    )
    .digest('hex');
}

function withAuthorization(init: RequestInit, token: string): RequestInit {
  return {
    ...init,
    headers: {
      ...init.headers,
      authorization: `Bearer ${token}`,
    },
  };
}

function assertReadOnlyPermissions(
  permissions: Record<string, 'read' | 'write'>,
  subject: string,
): void {
  const expected = Object.keys(INSTALLATION_PERMISSIONS).sort();
  const actual = Object.keys(permissions).sort();
  const exact =
    actual.length === expected.length &&
    actual.every(
      (permission, index) =>
        permission === expected[index] &&
        permissions[permission] ===
          INSTALLATION_PERMISSIONS[permission as keyof typeof INSTALLATION_PERMISSIONS],
    );
  if (exact) return;
  throw new PolicyError(
    'github_credential_invalid',
    `${subject} must have exactly the read permissions required by the signer`,
    503,
  );
}

function repositoryParts(repository: string): [string, string] {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository) || repository.length > 201) {
    throw new PolicyError('github_repository_invalid', 'Repository identity is invalid', 422);
  }
  return repository.split('/') as [string, string];
}

async function readJsonResponse(response: Response, limit: number): Promise<unknown> {
  if (mediaType(response) !== 'application/json') {
    throw new PolicyError(
      'github_invalid_response',
      'GitHub returned an invalid content type',
      503,
      true,
    );
  }
  const bytes = await readLimitedBody(response, limit, 'GitHub response');
  try {
    return JSON.parse(bytes.toString('utf8'));
  } catch {
    throw new PolicyError(
      'github_invalid_response',
      'GitHub response is not valid JSON',
      503,
      true,
    );
  }
}

async function readLimitedBody(
  response: Response,
  limit: number,
  subject: string,
): Promise<Buffer> {
  if (!response.body) {
    throw new PolicyError('github_invalid_response', `${subject} body is empty`, 503, true);
  }
  const reader = response.body.getReader();
  const chunks: Buffer[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      const code =
        subject === 'GitHub review artifact' ? 'github_diff_too_large' : 'github_invalid_response';
      const status = subject === 'GitHub review artifact' ? 422 : 503;
      throw new PolicyError(code, `${subject} is too large`, status, status === 503);
    }
    chunks.push(Buffer.from(value));
  }
  return Buffer.concat(chunks);
}

function mediaType(response: Response): string | undefined {
  return response.headers.get('content-type')?.split(';')[0]?.trim().toLowerCase();
}

function base64UrlJson(value: object): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url');
}
