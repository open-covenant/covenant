import { createHash, createSign } from 'node:crypto';
import { z } from 'zod';
import type { Config } from './config.js';
import { assertMaintenanceScope, parseIssueUrl } from './quote.js';
import type { GithubAuthorizationReceipt, GithubIssue, Job, RunArtifacts } from './types.js';

type Fetch = typeof fetch;
const GITHUB_TIMEOUT_MS = 20_000;
const githubAppSchema = z.object({
  id: z.number().int().positive(),
  slug: z.string().min(1).max(100),
});

export class GithubClient {
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
    const token = await this.installationToken(installationId);
    const root = `/repos/${job.quote.owner}/${job.quote.repo}`;
    const branch = `mizuki/${job.id.slice(0, 8)}`;
    const existingPullRequest = await this.existingPullRequest(job, branch, token);
    if (existingPullRequest) return existingPullRequest;

    const commit = await this.api<{ tree: { sha: string } }>(
      `${root}/git/commits/${job.quote.baseSha}`,
      { token },
    );
    let deliveryCommitSha = job.deliveryCommitSha;
    if (!deliveryCommitSha) {
      const existing = await this.api<{
        tree: Array<{ path: string; mode: string; type: string }>;
      }>(`${root}/git/trees/${commit.tree.sha}?recursive=1`, { token });
      const modes = new Map(existing.tree.map((entry) => [entry.path, entry.mode]));
      const content = new Map(artifacts.files.map((file) => [file.path, file.content]));
      const entries = [];
      for (const path of artifacts.changedFiles) {
        const value = content.get(path);
        if (value === undefined) throw new Error(`cannot publish deleted or binary file: ${path}`);
        const blob = await this.api<{ sha: string }>(`${root}/git/blobs`, {
          method: 'POST',
          token,
          body: { content: value, encoding: 'utf-8' },
        });
        entries.push({ path, mode: modes.get(path) ?? '100644', type: 'blob', sha: blob.sha });
      }
      const tree = await this.api<{ sha: string }>(`${root}/git/trees`, {
        method: 'POST',
        token,
        body: { base_tree: commit.tree.sha, tree: entries },
      });
      const created = await this.api<{ sha: string }>(`${root}/git/commits`, {
        method: 'POST',
        token,
        body: {
          message: `fix: ${job.quote.issueTitle}`.slice(0, 120),
          tree: tree.sha,
          parents: [job.quote.baseSha],
        },
      });
      deliveryCommitSha = created.sha;
      await checkpoint(deliveryCommitSha);
    }
    await this.ensureBranch(root, branch, deliveryCommitSha, token);
    try {
      const pr = await this.api<{ html_url: string }>(`${root}/pulls`, {
        method: 'POST',
        token,
        body: {
          title: job.quote.issueTitle,
          head: branch,
          base: job.quote.defaultBranch,
          body: `Closes #${job.quote.issueNumber}\n\nImplemented by Mizuki. Payment is retained only for delivered work.`,
        },
      });
      return pr.html_url;
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 422) throw cause;
      const existing = await this.existingPullRequest(job, branch, token);
      if (!existing) throw cause;
      return existing;
    }
  }

  async mergedAt(job: Job): Promise<string | undefined> {
    if (!job.prUrl || !job.quote.installationId) return undefined;
    const match = job.prUrl.match(/^https:\/\/github\.com\/[^/]+\/[^/]+\/pull\/(\d+)$/);
    if (!match) return undefined;
    const token = await this.installationToken(job.quote.installationId);
    const pull = await this.api<{ merged_at: string | null }>(
      `/repos/${job.quote.owner}/${job.quote.repo}/pulls/${match[1]}`,
      { token },
    );
    return pull.merged_at ?? undefined;
  }

  async pullRequestMergedAt(
    pullRequestUrl: string,
    installationId: number,
  ): Promise<string | undefined> {
    const parsed = parsePullRequestUrl(pullRequestUrl);
    const token = await this.installationToken(installationId);
    const pull = await this.api<{ merged_at: string | null }>(
      `/repos/${parsed.owner}/${parsed.repo}/pulls/${parsed.number}`,
      { token },
    );
    return pull.merged_at ?? undefined;
  }

  async pullRequestReviewData(
    pullRequestUrl: string,
    installationId: number,
  ): Promise<{
    diff: string;
    changedFiles: number;
    files: Array<{
      filename: string;
      previousFilename?: string;
      status: string;
      patchAvailable: boolean;
    }>;
    mergedAt?: string;
    checksPassed: boolean;
    checkCount: number;
  }> {
    const parsed = parsePullRequestUrl(pullRequestUrl);
    const token = await this.installationToken(installationId);
    const root = `/repos/${parsed.owner}/${parsed.repo}`;
    const pull = await this.api<{
      changed_files: number;
      merged_at: string | null;
      head: { sha: string };
    }>(`${root}/pulls/${parsed.number}`, { token });
    const [diff, checks, files] = await Promise.all([
      this.apiText(`${root}/pulls/${parsed.number}`, token, 'application/vnd.github.v3.diff'),
      this.api<{ check_runs: Array<{ status: string; conclusion: string | null }> }>(
        `${root}/commits/${pull.head.sha}/check-runs`,
        { token },
      ),
      this.api<
        Array<{
          filename: string;
          previous_filename?: string;
          status: string;
          patch?: string;
        }>
      >(`${root}/pulls/${parsed.number}/files?per_page=100`, { token }),
    ]);
    const accepted = new Set(['success', 'neutral', 'skipped']);
    const checksPassed = checks.check_runs.every(
      (check) => check.status === 'completed' && check.conclusion && accepted.has(check.conclusion),
    );
    return {
      diff,
      changedFiles: pull.changed_files,
      files: files.map((file) => ({
        filename: file.filename,
        ...(file.previous_filename ? { previousFilename: file.previous_filename } : {}),
        status: file.status,
        patchAvailable: typeof file.patch === 'string',
      })),
      mergedAt: pull.merged_at ?? undefined,
      checksPassed,
      checkCount: checks.check_runs.length,
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
    return ((await response.json()) as { id: number }).id;
  }

  private async authorizationReceipt(
    owner: string,
    repo: string,
    number: number,
    installationId: number,
    expectedIssue?: { title: string; body: string },
  ): Promise<GithubAuthorizationReceipt> {
    const token = await this.installationToken(installationId);
    const [issue, events] = await Promise.all([
      this.api<{
        title: string;
        body: string | null;
        labels: Array<{ name?: string }>;
        pull_request?: unknown;
      }>(`/repos/${owner}/${repo}/issues/${number}`, { token }),
      this.api<
        Array<{
          event?: string;
          created_at?: string;
          label?: { name?: string };
          actor?: { id?: number; login?: string; type?: string } | null;
        }>
      >(`/repos/${owner}/${repo}/issues/${number}/events?per_page=100`, { token }),
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
    const permissionResult = await this.api<{ permission?: string }>(
      `/repos/${owner}/${repo}/collaborators/${encodeURIComponent(event.actor.login)}/permission`,
      { token },
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
    token: string,
  ): Promise<string | undefined> {
    const owner = job.quote.owner;
    const root = `/repos/${owner}/${job.quote.repo}`;
    const query = new URLSearchParams({
      state: 'all',
      head: `${owner}:${branch}`,
      base: job.quote.defaultBranch,
      per_page: '2',
    });
    const pulls = await this.api<
      Array<{
        html_url: string;
        head: { ref: string };
        base: { ref: string };
      }>
    >(`${root}/pulls?${query}`, { token });
    return pulls.find(
      (pull) => pull.head.ref === branch && pull.base.ref === job.quote.defaultBranch,
    )?.html_url;
  }

  private async ensureBranch(
    root: string,
    branch: string,
    commitSha: string,
    token: string,
  ): Promise<void> {
    try {
      await this.api(`${root}/git/refs`, {
        method: 'POST',
        token,
        body: { ref: `refs/heads/${branch}`, sha: commitSha },
      });
      return;
    } catch (cause) {
      if (!(cause instanceof GithubApiError) || cause.status !== 422) throw cause;
    }
    const ref = await this.api<{ object: { sha: string } }>(
      `${root}/git/ref/heads/${encodeURIComponent(branch)}`,
      { token },
    );
    if (ref.object.sha !== commitSha) {
      throw new Error('existing delivery branch does not match the checkpointed commit');
    }
  }

  private async installationToken(id: number): Promise<string> {
    const result = await this.api<{ token: string }>(`/app/installations/${id}/access_tokens`, {
      method: 'POST',
      token: this.appJwt(),
    });
    return result.token;
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
      throw new Error(`GitHub GET ${path} failed: ${response.status}`);
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
