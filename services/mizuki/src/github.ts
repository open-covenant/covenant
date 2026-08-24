import { createHash, createSign } from 'node:crypto';
import { z } from 'zod';
import type { Config } from './config.js';
import { assertMaintenanceScope, parseIssueUrl } from './quote.js';
import type { GithubAuthorizationReceipt, GithubIssue, Job, RunArtifacts } from './types.js';

type Fetch = typeof fetch;
const GITHUB_TIMEOUT_MS = 20_000;
const TOKEN_REFRESH_WINDOW_MS = 5 * 60_000;
const TOKEN_MAX_TTL_MS = 65 * 60_000;
const TOKEN_CACHE_LIMIT = 256;
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
    const [repository, issue, contents] = await Promise.all([
      this.api<{ private: boolean; default_branch: string }>(`/repos/${owner}/${repo}`),
      this.api<{
        title: string;
        body: string | null;
        labels: Array<{ name?: string }>;
        pull_request?: unknown;
      }>(`/repos/${owner}/${repo}/issues/${number}`),
      this.api<Array<{ name: string }>>(`/repos/${owner}/${repo}/contents`),
    ]);
    if (repository.private) throw new Error('Mizuki v1 supports public repositories only');
    assertNotPullRequest(issue);

    const branch = await this.api<{ commit: { sha: string } }>(
      `/repos/${owner}/${repo}/branches/${encodeURIComponent(repository.default_branch)}`,
    );
    const installationId = await this.installation(owner, repo);
    if (this.config.requireGithubApp && installationId === undefined) {
      throw new Error('install the Mizuki GitHub App on this repository before requesting a quote');
    }
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

  async currentHead(owner: string, repo: string, branch: string): Promise<string> {
    const result = await this.api<{ commit: { sha: string } }>(
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
    const evidence = await this.pullRequestReviewData(pullRequestUrl, installationId);
    const reviewedDiffHash = createHash('sha256').update(artifacts.patch).digest('hex');
    if (
      evidence.headSha !== deliveryCommitSha ||
      evidence.baseSha !== job.quote.baseSha ||
      evidence.baseRef !== job.quote.defaultBranch ||
      evidence.diffHash !== reviewedDiffHash
    ) {
      throw new Error('published pull request does not match the reviewed delivery artifact');
    }
    await checkpoint({
      pullRequestNumber: pull.number,
      headSha: evidence.headSha,
      baseSha: evidence.baseSha,
      baseRef: evidence.baseRef,
      diffHash: evidence.diffHash,
      observedAt: new Date().toISOString(),
    });
    return pullRequestUrl;
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
      confirmed.merged_at !== pull.merged_at ||
      confirmed.merge_commit_sha !== pull.merge_commit_sha
    ) {
      throw new Error('pull request changed while review evidence was collected');
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
      if (this.config.requireGithubApp)
        throw new Error('GitHub App credentials are not configured');
      return undefined;
    }
    const response = await this.request(
      `https://api.github.com/repos/${owner}/${repo}/installation`,
      {
        headers: this.headers(this.appJwt()),
        signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
      },
    );
    if (response.status === 404) return undefined;
    if (!response.ok) throw new Error(`GitHub installation lookup failed: ${response.status}`);
    const installation = installationSchema.parse(await response.json());
    if (String(installation.app_id) !== this.config.githubAppId) {
      throw new Error('GitHub returned an installation for a different App');
    }
    if (installation.suspended_at) throw new Error('GitHub App installation is suspended');
    assertExactPermissions(installation.permissions, CORE_PERMISSIONS, 'GitHub App installation');
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
      throw new Error('authorization label has no attributable human maintainer event');
    }
    const permissionResult = await this.repositoryApi<{ permission?: string }>(
      owner,
      repo,
      installationId,
      `/repos/${owner}/${repo}/collaborators/${encodeURIComponent(event.actor.login)}/permission`,
    );
    const permission = permissionResult.permission;
    if (!isMaintainerPermission(permission)) {
      throw new Error('authorization label was not applied by a repository maintainer');
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
      if (!(cause instanceof GithubApiError) || cause.status !== 401) throw cause;
    }
    this.invalidateToken(owner, repo, installationId, first.token);
    const replacement = await this.installationToken(owner, repo, installationId);
    return this.api<T>(path, { ...options, token: replacement.token });
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
      if (!(cause instanceof GithubApiError) || cause.status !== 401) throw cause;
    }
    this.invalidateToken(owner, repo, installationId, first.token);
    const replacement = await this.installationToken(owner, repo, installationId);
    return this.apiText(path, replacement.token, accept);
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
    const response = await this.request(`https://api.github.com${path}`, {
      method: options.method,
      headers: this.headers(options.token),
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    if (!response.ok) {
      throw new GithubApiError(
        response.status,
        `GitHub request failed with HTTP ${response.status}`,
      );
    }
    return (await response.json()) as T;
  }

  private async apiText(path: string, token: string, accept: string): Promise<string> {
    const response = await this.request(`https://api.github.com${path}`, {
      headers: { ...this.headers(token), accept },
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
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
  const normalized = requiredLabel.trim().toLowerCase();
  if (!normalized || !labels.some((label) => label.trim().toLowerCase() === normalized)) {
    throw new Error(`issue must have the ${requiredLabel} label before Mizuki can act`);
  }
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

class GithubApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
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
