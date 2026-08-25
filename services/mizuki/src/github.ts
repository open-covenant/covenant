import { createHash, createSign } from 'node:crypto';
import { z } from 'zod';
import type { Config } from './config.js';
import { assertMaintenanceScope, createQuote, parseIssueUrl } from './quote.js';
import type { GithubAuthorizationReceipt, GithubIssue, Job, RunArtifacts } from './types.js';

type Fetch = typeof fetch;
const GITHUB_TIMEOUT_MS = 20_000;
const GITHUB_AUTH_UNAVAILABLE = 'Delivery GitHub App authentication is unavailable.';
const GITHUB_PROVENANCE_UNAVAILABLE = 'Delivery GitHub App configuration could not be verified.';
const GITHUB_REPOSITORY_UNAVAILABLE =
  'GitHub repository access is temporarily unavailable. Please try again shortly.';
const TOKEN_REFRESH_WINDOW_MS = 5 * 60_000;
const TOKEN_MAX_TTL_MS = 65 * 60_000;
const TOKEN_CACHE_LIMIT = 256;
const DELIVERY_DIFF_DOMAIN = 'mizuki.delivery-diff.v1\0';
const DIFF_INDEX_OBJECTS = /^index ([0-9a-f]{4,64})\.\.([0-9a-f]{4,64})( [0-7]{6})?$/gm;
const CORE_PERMISSIONS = {
  checks: 'read',
  contents: 'write',
  issues: 'read',
  metadata: 'read',
  pull_requests: 'write',
} as const;
const permissionSchema = z.record(z.string(), z.enum(['read', 'write']));
const githubAppSchema = z.object({
  id: z.number().int().positive(),
  slug: z.string().min(1).max(100),
  permissions: permissionSchema,
});
const installationSchema = z
  .object({
    id: z.number().int().positive(),
    app_id: z.number().int().positive(),
    repository_selection: z.literal('selected'),
    permissions: permissionSchema,
    suspended_at: z.string().datetime({ offset: true }).nullable().optional(),
  })
  .passthrough();
const installationTokenSchema = z
  .object({
    token: z.string().min(20).max(4_096).regex(/^\S+$/),
    expires_at: z.string().datetime({ offset: true }),
    permissions: z
      .object({
        checks: z.literal('read'),
        contents: z.literal('write'),
        issues: z.literal('read'),
        metadata: z.literal('read'),
        pull_requests: z.literal('write'),
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
const pullRequestReviewSchema = z.object({
  changed_files: z.number().int().nonnegative(),
  merged_at: z.string().datetime({ offset: true }).nullable(),
  merge_commit_sha: z
    .string()
    .regex(/^[a-f0-9]{40,64}$/)
    .nullable(),
  head: z.object({ sha: z.string().regex(/^[a-f0-9]{40,64}$/) }),
  base: z.object({
    sha: z.string().regex(/^[a-f0-9]{40,64}$/),
    ref: z.string().min(1).max(255),
  }),
});
const checkRunsSchema = z.object({
  total_count: z.number().int().nonnegative(),
  check_runs: z.array(
    z.object({
      status: z.string(),
      conclusion: z.string().nullable(),
    }),
  ),
});
const pullRequestFilesSchema = z.array(
  z.object({
    filename: z.string().min(1),
    previous_filename: z.string().min(1).optional(),
    status: z.string().min(1),
    patch: z.string().optional(),
  }),
);

interface CachedInstallationToken {
  token: string;
  expiresAt: number;
}

export type RepositoryMetadata = {
  owner: string;
  repo: string;
  repository: string;
  defaultBranch: string;
  installationId: number;
  permission: GithubAuthorizationReceipt['permission'];
};

export type RepositoryAccess = RepositoryMetadata & {
  rootFiles: string[];
};

export type WorkbenchIssue = {
  number: number;
  title: string;
  url: string;
  labels: string[];
  authorized: boolean;
  authorizationUnavailable?: boolean;
  scopeEligible: boolean;
  eligibility: boolean;
  class?: 'micro' | 'standard';
  priceAtomic?: string;
  maxFiles?: number;
  validationCommands: string[];
  reason?: string;
};

export type IssuePreflight = {
  owner: string;
  repo: string;
  repository: string;
  defaultBranch: string;
  core:
    | { status: 'ready'; installationId: number }
    | { status: 'action_required' }
    | { status: 'unavailable' };
  maintainer: {
    verified: boolean;
    unavailable?: boolean;
    permission?: GithubAuthorizationReceipt['permission'];
  };
  issue: WorkbenchIssue;
  blockers: string[];
};

export class GithubClient {
  private readonly tokenCache = new Map<string, CachedInstallationToken>();
  private readonly tokenRequests = new Map<string, Promise<CachedInstallationToken>>();

  constructor(
    private readonly config: Config,
    private readonly request: Fetch = fetch,
  ) {}

  async readiness(): Promise<void> {
    if (!this.config.githubAppId || !this.config.githubPrivateKey) {
      throw new Error('GitHub App credentials are not configured');
    }
    const configuredId = Number(this.config.githubAppId);
    if (!Number.isSafeInteger(configuredId) || configuredId <= 0) {
      throw new Error('GitHub App id is invalid');
    }
    const app = githubAppSchema.parse(await this.api('/app', { token: this.appJwt() }));
    if (app.id !== configuredId) throw new Error('GitHub authenticated a different App');
    assertExactPermissions(app.permissions, CORE_PERMISSIONS, 'GitHub App');
  }

  async issue(issueUrl: string): Promise<GithubIssue> {
    const { owner, repo, number } = parseIssueUrl(issueUrl);
    const installationId = await this.installation(owner, repo);
    if (this.config.requireGithubApp && installationId === undefined) {
      throw new GithubAccessError(
        'Install the Mizuki GitHub App on this repository before requesting a quote.',
      );
    }
    const repositoryPath = `/repos/${owner}/${repo}`;
    const read = <T>(path: string): Promise<T> =>
      installationId === undefined
        ? this.api<T>(path)
        : this.repositoryApi<T>(owner, repo, installationId, path);
    const [repository, issue, contents] = await Promise.all([
      read<{ private: boolean; default_branch: string }>(repositoryPath),
      read<{
        title: string;
        body: string | null;
        labels: Array<{ name?: string }>;
        pull_request?: unknown;
      }>(`${repositoryPath}/issues/${number}`),
      read<Array<{ name: string }>>(`${repositoryPath}/contents`),
    ]);
    if (repository.private) throw new Error('Mizuki v1 supports public repositories only');
    assertNotPullRequest(issue);

    const branch = await read<{ commit: { sha: string } }>(
      `${repositoryPath}/branches/${encodeURIComponent(repository.default_branch)}`,
    );
    if (this.config.requireGithubApp) {
      assertIssueAuthorized(
        issue.labels.flatMap((label) => (label.name ? [label.name] : [])),
        this.config.githubAuthorizationLabel,
      );
    }
    const currentIssue = {
      title: issue.title,
      body: issue.body ?? '',
      labels: issue.labels.flatMap((label) => (label.name ? [label.name] : [])),
    };
    assertMaintenanceScope(currentIssue);
    const authorizationReceipt = this.config.requireGithubApp
      ? await this.authorizationReceipt(owner, repo, number, installationId!, currentIssue)
      : undefined;

    return {
      owner,
      repo,
      number,
      ...currentIssue,
      defaultBranch: repository.default_branch,
      baseSha: branch.commit.sha,
      rootFiles: contents.map((entry) => entry.name),
      installationId,
      authorizationReceipt,
    };
  }

  async repositoryForMaintainer(
    ownerValue: string,
    repoValue: string,
    githubLogin: string,
  ): Promise<RepositoryAccess> {
    const metadata = await this.repositoryMetadataForMaintainer(ownerValue, repoValue, githubLogin);
    const contents = await this.repositoryApi<Array<{ name: string }>>(
      metadata.owner,
      metadata.repo,
      metadata.installationId,
      `/repos/${metadata.owner}/${metadata.repo}/contents`,
    );
    return { ...metadata, rootFiles: contents.map((entry) => entry.name) };
  }

  async repositoryMetadataForMaintainer(
    ownerValue: string,
    repoValue: string,
    githubLogin: string,
  ): Promise<RepositoryMetadata> {
    const { owner, repo } = repositoryIdentity(ownerValue, repoValue);
    const installationId = await this.installation(owner, repo);
    if (!installationId) {
      throw new GithubAccessError('install the Mizuki GitHub App on this repository first');
    }
    const [repository, permission] = await Promise.all([
      this.repositoryApi<{ private: boolean; default_branch: string }>(
        owner,
        repo,
        installationId,
        `/repos/${owner}/${repo}`,
      ),
      this.maintainerPermission(owner, repo, installationId, githubLogin),
    ]);
    if (repository.private) throw new GithubAccessError('private repositories are not supported');
    return {
      owner,
      repo,
      repository: `${owner}/${repo}`.toLowerCase(),
      defaultBranch: repository.default_branch,
      installationId,
      permission,
    };
  }

  async issuesForMaintainer(
    owner: string,
    repo: string,
    githubLogin: string,
  ): Promise<{ repository: RepositoryAccess; issues: WorkbenchIssue[] }> {
    const access = await this.repositoryForMaintainer(owner, repo, githubLogin);
    const [branch, issues] = await Promise.all([
      this.repositoryApi<{ commit: { sha: string } }>(
        access.owner,
        access.repo,
        access.installationId,
        `/repos/${access.owner}/${access.repo}/branches/${encodeURIComponent(access.defaultBranch)}`,
      ),
      this.repositoryApi<
        Array<{
          number: number;
          title: string;
          body: string | null;
          labels: Array<{ name?: string }>;
          pull_request?: unknown;
        }>
      >(
        access.owner,
        access.repo,
        access.installationId,
        `/repos/${access.owner}/${access.repo}/issues?state=open&sort=updated&direction=desc&per_page=20`,
      ),
    ]);
    const actorPermissions = new Map<string, Promise<void>>();
    return {
      repository: access,
      issues: await mapConcurrent(
        issues.filter((issue) => issue.pull_request === undefined),
        5,
        async (issue) => {
          let authorized = false;
          let authorizationUnavailable = false;
          let authorizationReason: string | undefined;
          const labelPresent = hasAuthorizationLabel(
            issue.labels,
            this.config.githubAuthorizationLabel,
          );
          const scoped = this.workbenchIssue(issue, access, branch.commit.sha, true);
          if (!scoped.eligibility) return { ...scoped, authorized: false };
          if (labelPresent) {
            try {
              await this.assertWorkbenchAuthorization(
                access.owner,
                access.repo,
                issue.number,
                access.installationId,
                actorPermissions,
              );
              authorized = true;
            } catch (cause) {
              if (cause instanceof GithubReadinessError) throw cause;
              authorizationUnavailable = githubAuthorizationUnavailable(cause);
              authorizationReason = authorizationUnavailable
                ? 'Issue authorization could not be verified. Try again shortly.'
                : `Have a maintainer remove and reapply the ${this.config.githubAuthorizationLabel} label.`;
            }
          }
          const result = this.workbenchIssue(
            issue,
            access,
            branch.commit.sha,
            authorized,
            authorizationReason,
          );
          return authorizationUnavailable ? { ...result, authorizationUnavailable: true } : result;
        },
      ),
    };
  }

  async preflightIssue(issueUrl: string, githubLogin: string): Promise<IssuePreflight> {
    const { owner, repo, number } = parseIssueUrl(issueUrl);
    const blockers: string[] = [];
    let installationId: number | undefined;
    let coreUnavailable = false;
    try {
      installationId = await this.installation(owner, repo);
    } catch (cause) {
      if (!(cause instanceof GithubReadinessError)) throw cause;
      if (this.config.requireGithubApp) throw cause;
      coreUnavailable = true;
      blockers.push(cause.message);
    }
    if (this.config.requireGithubApp && installationId === undefined) {
      throw new GithubAccessError('Install the Mizuki GitHub App on this repository first.');
    }
    if (!installationId && !coreUnavailable) {
      blockers.push('Install the delivery GitHub App on this repository.');
    }
    const repositoryPath = `/repos/${owner}/${repo}`;
    const [repository, issue, contents] = await Promise.all([
      installationId
        ? this.repositoryApi<{ private: boolean; default_branch: string }>(
            owner,
            repo,
            installationId,
            repositoryPath,
          )
        : this.api<{ private: boolean; default_branch: string }>(repositoryPath),
      installationId
        ? this.repositoryApi<{
            number: number;
            title: string;
            body: string | null;
            labels: Array<{ name?: string }>;
            pull_request?: unknown;
          }>(owner, repo, installationId, `${repositoryPath}/issues/${number}`)
        : this.api<{
            number: number;
            title: string;
            body: string | null;
            labels: Array<{ name?: string }>;
            pull_request?: unknown;
          }>(`${repositoryPath}/issues/${number}`),
      installationId
        ? this.repositoryApi<Array<{ name: string }>>(
            owner,
            repo,
            installationId,
            `${repositoryPath}/contents`,
          )
        : this.api<Array<{ name: string }>>(`${repositoryPath}/contents`),
    ]);
    if (repository.private) throw new GithubAccessError('private repositories are not supported');
    assertNotPullRequest(issue);
    const branchPath = `${repositoryPath}/branches/${encodeURIComponent(repository.default_branch)}`;
    const branch = installationId
      ? await this.repositoryApi<{ commit: { sha: string } }>(
          owner,
          repo,
          installationId,
          branchPath,
        )
      : await this.api<{ commit: { sha: string } }>(branchPath);
    const rootFiles = contents.map((entry) => entry.name);

    let permission: GithubAuthorizationReceipt['permission'] | undefined;
    let maintainerUnavailable = false;
    if (installationId) {
      try {
        permission = await this.maintainerPermission(owner, repo, installationId, githubLogin);
      } catch (cause) {
        if (cause instanceof GithubAccessError) throw cause;
        if (cause instanceof GithubReadinessError) throw cause;
        maintainerUnavailable = true;
        blockers.push('Repository maintainer access could not be verified. Try again shortly.');
      }
    }

    const issueRepository = {
      owner,
      repo,
      repository: `${owner}/${repo}`.toLowerCase(),
      defaultBranch: repository.default_branch,
      ...(installationId ? { installationId } : {}),
      rootFiles,
    };
    const scoped = this.workbenchIssue(issue, issueRepository, branch.commit.sha, true);
    const labelsPresent = hasAuthorizationLabel(issue.labels, this.config.githubAuthorizationLabel);
    let authorized = false;
    let authorizationUnavailable = false;
    let authorizationReason: string | undefined;
    if (scoped.eligibility) {
      if (!labelsPresent) {
        authorizationReason = `Add the ${this.config.githubAuthorizationLabel} label to the issue.`;
      } else if (!installationId) {
        authorizationUnavailable = coreUnavailable;
        authorizationReason = coreUnavailable
          ? 'Issue authorization could not be verified. Try again shortly.'
          : 'Install the delivery GitHub App to verify issue authorization.';
      } else {
        try {
          await this.authorizationReceipt(owner, repo, number, installationId, {
            title: issue.title,
            body: issue.body ?? '',
          });
          authorized = true;
        } catch (cause) {
          if (cause instanceof GithubReadinessError) throw cause;
          authorizationUnavailable = githubAuthorizationUnavailable(cause);
          authorizationReason = authorizationUnavailable
            ? 'Issue authorization could not be verified. Try again shortly.'
            : `Have a maintainer remove and reapply the ${this.config.githubAuthorizationLabel} label.`;
        }
      }
    }
    const workbenchIssue = scoped.eligibility
      ? this.workbenchIssue(
          issue,
          issueRepository,
          branch.commit.sha,
          authorized,
          authorizationReason,
        )
      : { ...scoped, authorized: false };
    const reportedIssue = authorizationUnavailable
      ? { ...workbenchIssue, authorizationUnavailable: true }
      : workbenchIssue;
    if (reportedIssue.reason && !blockers.includes(reportedIssue.reason)) {
      blockers.push(reportedIssue.reason);
    }
    return {
      owner,
      repo,
      repository: issueRepository.repository,
      defaultBranch: issueRepository.defaultBranch,
      core: installationId
        ? { status: 'ready', installationId }
        : { status: coreUnavailable ? 'unavailable' : 'action_required' },
      maintainer: {
        verified: permission !== undefined,
        ...(maintainerUnavailable ? { unavailable: true } : {}),
        ...(permission ? { permission } : {}),
      },
      issue: reportedIssue,
      blockers,
    };
  }

  async currentHead(
    owner: string,
    repo: string,
    branch: string,
    expectedInstallationId?: number,
  ): Promise<string> {
    const installationId = await this.installation(owner, repo);
    if (expectedInstallationId !== undefined && installationId !== expectedInstallationId) {
      throw new GithubAccessError('The GitHub App installation changed after the quote.');
    }
    if (this.config.requireGithubApp && expectedInstallationId === undefined) {
      throw new GithubReadinessError(
        'provenance',
        'GitHub App access could not be verified. Request a new quote before payment.',
      );
    }
    if (installationId === undefined) {
      if (this.config.requireGithubApp) {
        throw new GithubAccessError('The Mizuki GitHub App is not installed on this repository.');
      }
      const result = await this.api<{ commit: { sha: string } }>(
        `/repos/${owner}/${repo}/branches/${encodeURIComponent(branch)}`,
      );
      return result.commit.sha;
    }
    const result = await this.repositoryApi<{ commit: { sha: string } }>(
      owner,
      repo,
      installationId,
      `/repos/${owner}/${repo}/branches/${encodeURIComponent(branch)}`,
    );
    return result.commit.sha;
  }

  async assertIssueAuthorization(
    owner: string,
    repo: string,
    number: number,
    expectedInstallationId?: number,
    expectedEvidenceHash?: string,
    expectedIssue?: { title: string; body: string },
  ): Promise<GithubAuthorizationReceipt | undefined> {
    if (!this.config.requireGithubApp) return undefined;
    if (!expectedEvidenceHash) {
      throw new Error('quote does not contain immutable GitHub authorization evidence');
    }
    const installationId = await this.installation(owner, repo);
    if (!installationId || installationId !== expectedInstallationId) {
      throw new Error('Mizuki GitHub App installation changed after the quote');
    }
    const receipt = await this.authorizationReceipt(
      owner,
      repo,
      number,
      installationId,
      expectedIssue,
    );
    if (receipt.evidenceHash !== expectedEvidenceHash) {
      throw new Error('GitHub authorization changed after the quote; request a new quote');
    }
    return receipt;
  }

  async publish(
    job: Job,
    artifacts: RunArtifacts,
    checkpoint: (commitSha: string) => Promise<void> = async () => {},
    evidenceCheckpoint: (
      evidence: NonNullable<Job['deliveryEvidence']>,
    ) => Promise<void> = async () => {},
  ): Promise<string> {
    const installationId = job.quote.installationId;
    if (!installationId) throw new Error('GitHub App installation is required to publish a PR');
    await this.assertIssueAuthorization(
      job.quote.owner,
      job.quote.repo,
      job.quote.issueNumber,
      installationId,
      job.quote.authorizationReceipt?.evidenceHash,
      { title: job.quote.issueTitle, body: job.quote.issueBody },
    );
    const root = `/repos/${job.quote.owner}/${job.quote.repo}`;
    const branch = `mizuki/${job.id.slice(0, 8)}`;

    const commit = await this.repositoryApi<{ tree: { sha: string } }>(
      job.quote.owner,
      job.quote.repo,
      installationId,
      `${root}/git/commits/${job.quote.baseSha}`,
    );
    let deliveryCommitSha = job.deliveryCommitSha;
    if (!deliveryCommitSha) {
      const existing = await this.repositoryApi<{
        tree: Array<{ path: string; mode: string; type: string }>;
      }>(
        job.quote.owner,
        job.quote.repo,
        installationId,
        `${root}/git/trees/${commit.tree.sha}?recursive=1`,
      );
      const modes = new Map(existing.tree.map((entry) => [entry.path, entry.mode]));
      const content = new Map(artifacts.files.map((file) => [file.path, file.content]));
      const entries = [];
      for (const path of artifacts.changedFiles) {
        const value = content.get(path);
        if (value === undefined) throw new Error(`cannot publish deleted or binary file: ${path}`);
        const blob = await this.repositoryApi<{ sha: string }>(
          job.quote.owner,
          job.quote.repo,
          installationId,
          `${root}/git/blobs`,
          {
            method: 'POST',
            body: { content: value, encoding: 'utf-8' },
          },
        );
        entries.push({ path, mode: modes.get(path) ?? '100644', type: 'blob', sha: blob.sha });
      }
      const tree = await this.repositoryApi<{ sha: string }>(
        job.quote.owner,
        job.quote.repo,
        installationId,
        `${root}/git/trees`,
        {
          method: 'POST',
          body: { base_tree: commit.tree.sha, tree: entries },
        },
      );
      const created = await this.repositoryApi<{ sha: string }>(
        job.quote.owner,
        job.quote.repo,
        installationId,
        `${root}/git/commits`,
        {
          method: 'POST',
          body: {
            message: `fix: ${job.quote.issueTitle}`.slice(0, 120),
            tree: tree.sha,
            parents: [job.quote.baseSha],
          },
        },
      );
      deliveryCommitSha = created.sha;
    }
    await checkpoint(deliveryCommitSha);
    await this.ensureBranch(job, root, branch, deliveryCommitSha);
    const existingPullRequest = await this.existingPullRequest(job, branch, deliveryCommitSha);
    if (existingPullRequest) {
      return this.captureDeliveryEvidence(
        job,
        artifacts,
        existingPullRequest,
        deliveryCommitSha,
        evidenceCheckpoint,
      );
    }
    try {
      const pr = await this.repositoryApi<{ html_url: string }>(
        job.quote.owner,
        job.quote.repo,
        installationId,
        `${root}/pulls`,
        {
          method: 'POST',
          body: {
            title: job.quote.issueTitle,
            head: branch,
            base: job.quote.defaultBranch,
            body: `Closes #${job.quote.issueNumber}\n\nImplemented by Mizuki. Payment is retained only for delivered work.`,
          },
        },
      );
      return this.captureDeliveryEvidence(
        job,
        artifacts,
        pr.html_url,
        deliveryCommitSha,
        evidenceCheckpoint,
      );
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 422) throw cause;
      const existing = await this.existingPullRequest(job, branch, deliveryCommitSha);
      if (!existing) throw cause;
      return this.captureDeliveryEvidence(
        job,
        artifacts,
        existing,
        deliveryCommitSha,
        evidenceCheckpoint,
      );
    }
  }

  private async captureDeliveryEvidence(
    job: Job,
    artifacts: RunArtifacts,
    pullRequestUrl: string,
    deliveryCommitSha: string,
    checkpoint: (evidence: NonNullable<Job['deliveryEvidence']>) => Promise<void>,
  ): Promise<string> {
    const installationId = job.quote.installationId!;
    const pull = parsePullRequestUrl(pullRequestUrl);
    const evidence = await this.stablePullRequestReviewData(pullRequestUrl, installationId);
    const reviewedDiffHash = deliveryDiffHash(artifacts.patch);
    const publishedDiffHash = deliveryDiffHash(evidence.diff);
    if (
      evidence.headSha !== deliveryCommitSha ||
      evidence.baseSha !== job.quote.baseSha ||
      evidence.baseRef !== job.quote.defaultBranch ||
      publishedDiffHash !== reviewedDiffHash
    ) {
      throw new Error('published pull request does not match the reviewed delivery artifact');
    }
    await checkpoint({
      pullRequestNumber: pull.number,
      headSha: evidence.headSha,
      baseSha: evidence.baseSha,
      baseRef: evidence.baseRef,
      diffHash: publishedDiffHash,
      observedAt: new Date().toISOString(),
    });
    return pullRequestUrl;
  }

  private async stablePullRequestReviewData(
    pullRequestUrl: string,
    installationId: number,
  ): Promise<Awaited<ReturnType<GithubClient['pullRequestReviewData']>>> {
    try {
      return await this.pullRequestReviewData(pullRequestUrl, installationId);
    } catch (cause) {
      if (!(cause instanceof PullRequestMergeMetadataChangedError)) throw cause;
      return this.pullRequestReviewData(pullRequestUrl, installationId);
    }
  }

  async mergedAt(job: Job): Promise<string | undefined> {
    if (!job.prUrl || !job.quote.installationId) return undefined;
    const match = job.prUrl.match(/^https:\/\/github\.com\/[^/]+\/[^/]+\/pull\/(\d+)$/);
    if (!match) return undefined;
    const pull = await this.repositoryApi<{ merged_at: string | null }>(
      job.quote.owner,
      job.quote.repo,
      job.quote.installationId,
      `/repos/${job.quote.owner}/${job.quote.repo}/pulls/${match[1]}`,
    );
    return pull.merged_at ?? undefined;
  }

  async pullRequestMergedAt(
    pullRequestUrl: string,
    installationId: number,
  ): Promise<string | undefined> {
    const parsed = parsePullRequestUrl(pullRequestUrl);
    const pull = await this.repositoryApi<{ merged_at: string | null }>(
      parsed.owner,
      parsed.repo,
      installationId,
      `/repos/${parsed.owner}/${parsed.repo}/pulls/${parsed.number}`,
    );
    return pull.merged_at ?? undefined;
  }

  async pullRequestReviewData(
    pullRequestUrl: string,
    installationId: number,
  ): Promise<{
    headSha: string;
    baseSha: string;
    baseRef: string;
    diffHash: string;
    diff: string;
    changedFiles: number;
    files: Array<{
      filename: string;
      previousFilename?: string;
      status: string;
      patchAvailable: boolean;
    }>;
    mergedAt?: string;
    mergeCommitSha?: string;
    checksPassed: boolean;
    checkCount: number;
  }> {
    const parsed = parsePullRequestUrl(pullRequestUrl);
    const root = `/repos/${parsed.owner}/${parsed.repo}`;
    const pull = pullRequestReviewSchema.parse(
      await this.repositoryApi(
        parsed.owner,
        parsed.repo,
        installationId,
        `${root}/pulls/${parsed.number}`,
      ),
    );
    const [diff, rawChecks, rawFiles] = await Promise.all([
      this.repositoryApiText(
        parsed.owner,
        parsed.repo,
        installationId,
        `${root}/pulls/${parsed.number}`,
        'application/vnd.github.v3.diff',
      ),
      this.repositoryApi(
        parsed.owner,
        parsed.repo,
        installationId,
        `${root}/commits/${pull.merge_commit_sha ?? pull.head.sha}/check-runs?per_page=100`,
      ),
      this.repositoryApi(
        parsed.owner,
        parsed.repo,
        installationId,
        `${root}/pulls/${parsed.number}/files?per_page=100`,
      ),
    ]);
    const checks = checkRunsSchema.parse(rawChecks);
    const files = pullRequestFilesSchema.parse(rawFiles);
    const confirmed = pullRequestReviewSchema.parse(
      await this.repositoryApi(
        parsed.owner,
        parsed.repo,
        installationId,
        `${root}/pulls/${parsed.number}`,
      ),
    );
    if (
      confirmed.head.sha !== pull.head.sha ||
      confirmed.base.sha !== pull.base.sha ||
      confirmed.base.ref !== pull.base.ref ||
      confirmed.changed_files !== pull.changed_files ||
      confirmed.merged_at !== pull.merged_at
    ) {
      throw new Error('pull request changed while review evidence was collected');
    }
    if (confirmed.merge_commit_sha !== pull.merge_commit_sha) {
      throw new PullRequestMergeMetadataChangedError();
    }
    const checksPassed =
      checks.total_count === checks.check_runs.length &&
      checks.check_runs.every(
        (check) => check.status === 'completed' && check.conclusion === 'success',
      );
    return {
      headSha: confirmed.head.sha,
      baseSha: confirmed.base.sha,
      baseRef: confirmed.base.ref,
      diffHash: createHash('sha256').update(diff).digest('hex'),
      diff,
      changedFiles: confirmed.changed_files,
      files: files.map((file) => ({
        filename: file.filename,
        ...(file.previous_filename ? { previousFilename: file.previous_filename } : {}),
        status: file.status,
        patchAvailable: typeof file.patch === 'string',
      })),
      mergedAt: confirmed.merged_at ?? undefined,
      ...(confirmed.merge_commit_sha ? { mergeCommitSha: confirmed.merge_commit_sha } : {}),
      checksPassed,
      checkCount: checks.total_count,
    };
  }

  private async installation(owner: string, repo: string): Promise<number | undefined> {
    if (!this.config.githubAppId || !this.config.githubPrivateKey) {
      if (this.config.requireGithubApp) {
        throw new GithubReadinessError('credentials', GITHUB_AUTH_UNAVAILABLE);
      }
      return undefined;
    }
    let response: Response;
    try {
      response = await this.request(`https://api.github.com/repos/${owner}/${repo}/installation`, {
        headers: this.headers(this.appJwt()),
        signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
      });
    } catch {
      throw new GithubReadinessError('unavailable', GITHUB_REPOSITORY_UNAVAILABLE);
    }
    if (response.status === 404) return undefined;
    if (!response.ok) throw githubReadinessFromStatus(response.status);
    let installation: z.infer<typeof installationSchema>;
    try {
      installation = installationSchema.parse(await response.json());
    } catch {
      throw new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE);
    }
    if (String(installation.app_id) !== this.config.githubAppId) {
      throw new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE);
    }
    if (installation.suspended_at) {
      throw new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE);
    }
    try {
      assertExactPermissions(installation.permissions, CORE_PERMISSIONS, 'GitHub App installation');
    } catch {
      throw new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE);
    }
    return installation.id;
  }

  private async authorizationReceipt(
    owner: string,
    repo: string,
    number: number,
    installationId: number,
    expectedIssue?: { title: string; body: string },
  ): Promise<GithubAuthorizationReceipt> {
    const [issue, events] = await Promise.all([
      this.repositoryApi<{
        title: string;
        body: string | null;
        labels: Array<{ name?: string }>;
        pull_request?: unknown;
      }>(owner, repo, installationId, `/repos/${owner}/${repo}/issues/${number}`),
      this.repositoryApi<
        Array<{
          event?: string;
          created_at?: string;
          label?: { name?: string };
          actor?: { id?: number; login?: string; type?: string } | null;
        }>
      >(
        owner,
        repo,
        installationId,
        `/repos/${owner}/${repo}/issues/${number}/events?per_page=100`,
      ),
    ]);
    assertNotPullRequest(issue);
    const currentIssue = {
      title: issue.title,
      body: issue.body ?? '',
      labels: issue.labels.flatMap((label) => (label.name ? [label.name] : [])),
    };
    assertMaintenanceScope(currentIssue);
    if (
      expectedIssue &&
      (currentIssue.title !== expectedIssue.title || currentIssue.body !== expectedIssue.body)
    ) {
      throw new Error('GitHub issue changed after the quote; request a new quote');
    }
    assertIssueAuthorized(currentIssue.labels, this.config.githubAuthorizationLabel);
    const requiredLabel = this.config.githubAuthorizationLabel.trim().toLowerCase();
    const event = events
      .filter(
        (candidate) =>
          candidate.event === 'labeled' &&
          candidate.label?.name?.trim().toLowerCase() === requiredLabel,
      )
      .sort((left, right) => String(right.created_at).localeCompare(String(left.created_at)))[0];
    if (
      !event?.created_at ||
      !Number.isFinite(Date.parse(event.created_at)) ||
      !event.actor?.id ||
      !event.actor.login ||
      event.actor.type !== 'User'
    ) {
      throw new GithubAuthorizationError(
        'authorization label has no attributable human maintainer event',
      );
    }
    const permissionResult = await this.repositoryApi<{ permission?: string }>(
      owner,
      repo,
      installationId,
      `/repos/${owner}/${repo}/collaborators/${encodeURIComponent(event.actor.login)}/permission`,
    );
    const permission = permissionResult.permission;
    if (!isMaintainerPermission(permission)) {
      throw new GithubAuthorizationError(
        'authorization label was not applied by a repository maintainer',
      );
    }
    const evidence = {
      owner: owner.toLowerCase(),
      repo: repo.toLowerCase(),
      issueNumber: number,
      installationId,
      label: requiredLabel,
      actorId: String(event.actor.id),
      actorLogin: event.actor.login.toLowerCase(),
      permission,
      authorizedAt: new Date(event.created_at).toISOString(),
    };
    return {
      label: evidence.label,
      actorId: evidence.actorId,
      actorLogin: evidence.actorLogin,
      permission,
      authorizedAt: evidence.authorizedAt,
      verifiedAt: new Date().toISOString(),
      evidenceHash: createHash('sha256').update(JSON.stringify(evidence)).digest('hex'),
    };
  }

  private async maintainerPermission(
    owner: string,
    repo: string,
    installationId: number,
    githubLogin: string,
  ): Promise<GithubAuthorizationReceipt['permission']> {
    if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$/.test(githubLogin)) {
      throw new GithubAccessError('GitHub account identity is invalid');
    }
    let result: { permission?: string };
    try {
      result = await this.repositoryApi<{ permission?: string }>(
        owner,
        repo,
        installationId,
        `/repos/${owner}/${repo}/collaborators/${encodeURIComponent(githubLogin)}/permission`,
      );
    } catch (cause) {
      if (cause instanceof GithubApiError && cause.status === 404) {
        throw new GithubAccessError('repository maintainer access is required');
      }
      throw cause;
    }
    if (!isMaintainerPermission(result.permission)) {
      throw new GithubAccessError('repository maintainer access is required');
    }
    return result.permission;
  }

  private async assertWorkbenchAuthorization(
    owner: string,
    repo: string,
    number: number,
    installationId: number,
    permissions: Map<string, Promise<void>>,
  ): Promise<void> {
    const events = await this.repositoryApi<
      Array<{
        event?: string;
        created_at?: string;
        label?: { name?: string };
        actor?: { id?: number; login?: string; type?: string } | null;
      }>
    >(owner, repo, installationId, `/repos/${owner}/${repo}/issues/${number}/events?per_page=100`);
    const requiredLabel = this.config.githubAuthorizationLabel.trim().toLowerCase();
    const event = events
      .filter(
        (candidate) =>
          candidate.event === 'labeled' &&
          candidate.label?.name?.trim().toLowerCase() === requiredLabel,
      )
      .sort((left, right) => String(right.created_at).localeCompare(String(left.created_at)))[0];
    if (
      !event?.created_at ||
      !Number.isFinite(Date.parse(event.created_at)) ||
      !event.actor?.id ||
      !event.actor.login ||
      event.actor.type !== 'User'
    ) {
      throw new GithubAuthorizationError(
        'authorization label has no attributable human maintainer event',
      );
    }
    const login = event.actor.login.toLowerCase();
    let permission = permissions.get(login);
    if (!permission) {
      permission = this.repositoryApi<{ permission?: string }>(
        owner,
        repo,
        installationId,
        `/repos/${owner}/${repo}/collaborators/${encodeURIComponent(event.actor.login)}/permission`,
      ).then((result) => {
        if (!isMaintainerPermission(result.permission)) {
          throw new GithubAuthorizationError(
            'authorization label was not applied by a repository maintainer',
          );
        }
      });
      permissions.set(login, permission);
    }
    await permission;
  }

  private workbenchIssue(
    issue: {
      number: number;
      title: string;
      body: string | null;
      labels: Array<{ name?: string }>;
    },
    repository: Pick<
      RepositoryAccess,
      'owner' | 'repo' | 'repository' | 'defaultBranch' | 'rootFiles'
    > & { installationId?: number },
    baseSha: string,
    authorized: boolean,
    authorizationReason?: string,
  ): WorkbenchIssue {
    const labels = issue.labels.flatMap((label) => (label.name ? [label.name] : []));
    const base = {
      number: issue.number,
      title: issue.title,
      url: `https://github.com/${repository.owner}/${repository.repo}/issues/${issue.number}`,
      labels,
      authorized,
      validationCommands: [] as string[],
    };
    let quote: ReturnType<typeof createQuote>;
    try {
      quote = createQuote({
        owner: repository.owner,
        repo: repository.repo,
        number: issue.number,
        title: issue.title,
        body: issue.body ?? '',
        labels,
        defaultBranch: repository.defaultBranch,
        baseSha,
        rootFiles: repository.rootFiles,
        installationId: repository.installationId,
      });
    } catch (cause) {
      return {
        ...base,
        scopeEligible: false,
        eligibility: false,
        reason: publicEligibilityReason(cause),
      };
    }
    const quoteDetails = {
      class: quote.class,
      priceAtomic: quote.priceAtomic,
      maxFiles: quote.maxFiles,
      validationCommands: quote.validationCommands,
    };
    if (!authorized) {
      return {
        ...base,
        ...quoteDetails,
        scopeEligible: true,
        eligibility: false,
        reason:
          authorizationReason ??
          `Add the ${this.config.githubAuthorizationLabel} label to authorize work.`,
      };
    }
    return { ...base, ...quoteDetails, scopeEligible: true, eligibility: true };
  }

  private async existingPullRequest(
    job: Job,
    branch: string,
    deliveryCommitSha: string,
  ): Promise<string | undefined> {
    const owner = job.quote.owner;
    const root = `/repos/${owner}/${job.quote.repo}`;
    const query = new URLSearchParams({
      state: 'all',
      head: `${owner}:${branch}`,
      base: job.quote.defaultBranch,
      per_page: '2',
    });
    const pulls = await this.repositoryApi<
      Array<{
        html_url: string;
        head: { ref: string; sha: string };
        base: { ref: string };
      }>
    >(owner, job.quote.repo, job.quote.installationId!, `${root}/pulls?${query}`);
    return pulls.find(
      (pull) =>
        pull.head.ref === branch &&
        pull.head.sha === deliveryCommitSha &&
        pull.base.ref === job.quote.defaultBranch,
    )?.html_url;
  }

  private async ensureBranch(
    job: Job,
    root: string,
    branch: string,
    commitSha: string,
  ): Promise<void> {
    try {
      await this.repositoryApi(
        job.quote.owner,
        job.quote.repo,
        job.quote.installationId!,
        `${root}/git/refs`,
        {
          method: 'POST',
          body: { ref: `refs/heads/${branch}`, sha: commitSha },
        },
      );
      return;
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 422) throw cause;
    }
    const ref = await this.repositoryApi<{ object: { sha: string } }>(
      job.quote.owner,
      job.quote.repo,
      job.quote.installationId!,
      `${root}/git/ref/heads/${encodeURIComponent(branch)}`,
    );
    if (ref.object.sha !== commitSha) {
      throw new Error('existing delivery branch does not match the checkpointed commit');
    }
  }

  private async installationToken(
    owner: string,
    repo: string,
    id: number,
  ): Promise<CachedInstallationToken> {
    const key = tokenCacheKey(owner, repo, id);
    const cached = this.tokenCache.get(key);
    if (cached && cached.expiresAt - Date.now() > TOKEN_REFRESH_WINDOW_MS) {
      this.tokenCache.delete(key);
      this.tokenCache.set(key, cached);
      return cached;
    }
    if (cached) this.tokenCache.delete(key);

    const pending = this.tokenRequests.get(key);
    if (pending) return pending;
    const request = this.mintInstallationToken(owner, repo, id).finally(() => {
      if (this.tokenRequests.get(key) === request) this.tokenRequests.delete(key);
    });
    this.tokenRequests.set(key, request);
    return request;
  }

  private async mintInstallationToken(
    owner: string,
    repo: string,
    id: number,
  ): Promise<CachedInstallationToken> {
    try {
      return await this.mintInstallationTokenUnchecked(owner, repo, id);
    } catch (cause) {
      if (cause instanceof GithubReadinessError) throw cause;
      if (cause instanceof GithubApiError) throw githubReadinessFromStatus(cause.status);
      throw new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE);
    }
  }

  private async mintInstallationTokenUnchecked(
    owner: string,
    repo: string,
    id: number,
  ): Promise<CachedInstallationToken> {
    const installation = installationSchema.parse(
      await this.api(`/repos/${owner}/${repo}/installation`, { token: this.appJwt() }),
    );
    if (installation.id !== id || String(installation.app_id) !== this.config.githubAppId) {
      throw new Error('GitHub returned an installation for a different App or repository');
    }
    if (installation.suspended_at) throw new Error('GitHub App installation is suspended');
    assertExactPermissions(installation.permissions, CORE_PERMISSIONS, 'GitHub App installation');
    const result = installationTokenSchema.parse(
      await this.api(`/app/installations/${id}/access_tokens`, {
        method: 'POST',
        token: this.appJwt(),
        body: { repositories: [repo], permissions: CORE_PERMISSIONS },
      }),
    );
    if (
      result.repositories[0]?.name.toLowerCase() !== repo.toLowerCase() ||
      result.repositories[0]?.full_name.toLowerCase() !== `${owner}/${repo}`.toLowerCase()
    ) {
      throw new Error('GitHub returned an incorrectly scoped repository token');
    }
    const expiresAt = new Date(result.expires_at).getTime();
    const ttl = expiresAt - Date.now();
    if (ttl <= TOKEN_REFRESH_WINDOW_MS || ttl > TOKEN_MAX_TTL_MS) {
      throw new Error('GitHub returned an invalid repository token lifetime');
    }
    const token = { token: result.token, expiresAt };
    if (
      !this.tokenCache.has(tokenCacheKey(owner, repo, id)) &&
      this.tokenCache.size >= TOKEN_CACHE_LIMIT
    ) {
      const oldest = this.tokenCache.keys().next().value;
      if (oldest) this.tokenCache.delete(oldest);
    }
    this.tokenCache.set(tokenCacheKey(owner, repo, id), token);
    return token;
  }

  private invalidateToken(owner: string, repo: string, id: number, token: string): void {
    const key = tokenCacheKey(owner, repo, id);
    if (this.tokenCache.get(key)?.token === token) this.tokenCache.delete(key);
  }

  private async repositoryApi<T = unknown>(
    owner: string,
    repo: string,
    installationId: number,
    path: string,
    options: { method?: string; body?: unknown } = {},
  ): Promise<T> {
    const first = await this.installationToken(owner, repo, installationId);
    try {
      return await this.api<T>(path, { ...options, token: first.token });
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 401) {
        throw operationalGithubError(cause);
      }
    }
    this.invalidateToken(owner, repo, installationId, first.token);
    const replacement = await this.installationToken(owner, repo, installationId);
    try {
      return await this.api<T>(path, { ...options, token: replacement.token });
    } catch (cause) {
      throw operationalGithubError(cause);
    }
  }

  private async repositoryApiText(
    owner: string,
    repo: string,
    installationId: number,
    path: string,
    accept: string,
  ): Promise<string> {
    const first = await this.installationToken(owner, repo, installationId);
    try {
      return await this.apiText(path, first.token, accept);
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 401) {
        throw operationalGithubError(cause);
      }
    }
    this.invalidateToken(owner, repo, installationId, first.token);
    const replacement = await this.installationToken(owner, repo, installationId);
    try {
      return await this.apiText(path, replacement.token, accept);
    } catch (cause) {
      throw operationalGithubError(cause);
    }
  }

  private appJwt(): string {
    if (!this.config.githubAppId || !this.config.githubPrivateKey) {
      throw new Error('GitHub App credentials are not configured');
    }
    const now = Math.floor(Date.now() / 1_000);
    const header = encode({ alg: 'RS256', typ: 'JWT' });
    const payload = encode({ iat: now - 60, exp: now + 540, iss: this.config.githubAppId });
    const unsigned = `${header}.${payload}`;
    const signer = createSign('RSA-SHA256');
    signer.update(unsigned);
    return `${unsigned}.${signer.sign(this.config.githubPrivateKey, 'base64url')}`;
  }

  private async api<T = unknown>(
    path: string,
    options: { method?: string; token?: string; body?: unknown } = {},
  ): Promise<T> {
    let response: Response;
    try {
      response = await this.request(`https://api.github.com${path}`, {
        method: options.method,
        headers: this.headers(options.token),
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
      });
    } catch {
      throw new GithubReadinessError('unavailable', GITHUB_REPOSITORY_UNAVAILABLE);
    }
    if (!response.ok) {
      throw new GithubApiError(
        response.status,
        `GitHub request failed with HTTP ${response.status}`,
      );
    }
    try {
      return (await response.json()) as T;
    } catch {
      throw new GithubReadinessError('unavailable', GITHUB_REPOSITORY_UNAVAILABLE);
    }
  }

  private async apiText(path: string, token: string, accept: string): Promise<string> {
    let response: Response;
    try {
      response = await this.request(`https://api.github.com${path}`, {
        headers: { ...this.headers(token), accept },
        signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
      });
    } catch {
      throw new GithubReadinessError('unavailable', GITHUB_REPOSITORY_UNAVAILABLE);
    }
    if (!response.ok) {
      throw new GithubApiError(response.status, `GitHub GET ${path} failed: ${response.status}`);
    }
    const text = await response.text();
    if (text.length > 2_000_000) throw new Error('pull request diff exceeds review limit');
    return text;
  }

  private headers(token?: string): Record<string, string> {
    return {
      accept: 'application/vnd.github+json',
      'content-type': 'application/json',
      'user-agent': 'mizuki-maintainer',
      'x-github-api-version': '2022-11-28',
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    };
  }
}

export function assertIssueAuthorized(labels: string[], requiredLabel: string): void {
  if (
    !hasAuthorizationLabel(
      labels.map((name) => ({ name })),
      requiredLabel,
    )
  ) {
    throw new Error(`issue must have the ${requiredLabel} label before Mizuki can act`);
  }
}

function hasAuthorizationLabel(labels: Array<{ name?: string }>, requiredLabel: string): boolean {
  const normalized = requiredLabel.trim().toLowerCase();
  return Boolean(
    normalized && labels.some((label) => label.name?.trim().toLowerCase() === normalized),
  );
}

function repositoryIdentity(owner: string, repo: string): { owner: string; repo: string } {
  const segment = /^[A-Za-z0-9_.-]{1,100}$/;
  if (!segment.test(owner) || !segment.test(repo)) {
    throw new GithubAccessError('repository identity is invalid');
  }
  return { owner, repo };
}

function publicEligibilityReason(cause: unknown): string {
  const message = cause instanceof Error ? cause.message : '';
  if (/supported deterministic validation command/i.test(message)) {
    return 'This repository does not expose a supported validation command.';
  }
  if (/maintenance-only scope|outside Mizuki|safe MVP scope|too large/i.test(message)) {
    return 'This issue is outside the supported maintenance scope.';
  }
  return 'This issue is not eligible for automated maintenance.';
}

async function mapConcurrent<T, R>(
  values: T[],
  concurrency: number,
  operation: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (next < values.length) {
      const index = next;
      next += 1;
      results[index] = await operation(values[index]!);
    }
  });
  await Promise.all(workers);
  return results;
}

function assertNotPullRequest(issue: { pull_request?: unknown }): void {
  if (issue.pull_request !== undefined) {
    throw new Error('pull request URLs cannot be submitted as maintenance issues');
  }
}

function isMaintainerPermission(
  value: string | undefined,
): value is GithubAuthorizationReceipt['permission'] {
  return value === 'triage' || value === 'write' || value === 'maintain' || value === 'admin';
}

function assertExactPermissions<T extends Record<string, 'read' | 'write'>>(
  actual: Record<string, 'read' | 'write'>,
  expected: T,
  subject: string,
): void {
  const actualEntries = Object.entries(actual).sort(([left], [right]) => left.localeCompare(right));
  const expectedEntries = Object.entries(expected).sort(([left], [right]) =>
    left.localeCompare(right),
  );
  if (
    actualEntries.length === expectedEntries.length &&
    actualEntries.every(
      ([name, permission], index) =>
        name === expectedEntries[index]?.[0] && permission === expectedEntries[index]?.[1],
    )
  ) {
    return;
  }
  throw new Error(`${subject} permissions do not match the required delivery contract`);
}

function tokenCacheKey(owner: string, repo: string, installationId: number): string {
  return `${installationId}:${owner.toLowerCase()}/${repo.toLowerCase()}`;
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

class GithubApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

class PullRequestMergeMetadataChangedError extends Error {
  constructor() {
    super('pull request changed while review evidence was collected');
  }
}

export class GithubAccessError extends Error {}

export class GithubReadinessError extends Error {
  constructor(
    readonly code: 'credentials' | 'provenance' | 'unavailable',
    message: string,
    readonly upstreamStatus?: number,
  ) {
    super(message);
  }
}

class GithubAuthorizationError extends Error {}

function githubReadinessFromStatus(status: number): GithubReadinessError {
  if (status === 401) {
    return new GithubReadinessError('credentials', GITHUB_AUTH_UNAVAILABLE, status);
  }
  if (status === 403 || status === 429 || status >= 500) {
    return new GithubReadinessError('unavailable', GITHUB_REPOSITORY_UNAVAILABLE, status);
  }
  return new GithubReadinessError('provenance', GITHUB_PROVENANCE_UNAVAILABLE, status);
}

function operationalGithubError(cause: unknown): unknown {
  if (cause instanceof GithubReadinessError) return cause;
  if (
    cause instanceof GithubApiError &&
    (cause.status === 401 || cause.status === 403 || cause.status === 429 || cause.status >= 500)
  ) {
    return githubReadinessFromStatus(cause.status);
  }
  return cause;
}

function githubAuthorizationUnavailable(cause: unknown): boolean {
  if (cause instanceof GithubReadinessError || cause instanceof GithubApiError) return true;
  return !(cause instanceof GithubAuthorizationError || cause instanceof GithubAccessError);
}

function encode(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url');
}

export function parsePullRequestUrl(value: string): {
  owner: string;
  repo: string;
  number: number;
} {
  const match = value.match(
    /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/pull\/(\d+)(?:[/?#].*)?$/,
  );
  if (!match) throw new Error('expected a canonical GitHub pull request URL');
  return { owner: match[1]!, repo: match[2]!, number: Number(match[3]) };
}
