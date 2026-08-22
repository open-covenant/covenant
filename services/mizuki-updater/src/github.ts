import { createPrivateKey, createSign } from 'node:crypto';
import { z } from 'zod';
import type { UpgradeManifest } from './domain.js';
import { UpdaterError } from './domain.js';

export interface PullRequestReceipt {
  number: number;
  url: string;
}

export interface CheckReceipt {
  status: 'pending' | 'passed' | 'failed';
  checks: Record<string, string>;
}

export interface MergeReceipt {
  mergeSha: string;
}

export interface GitHubGateway {
  syncPullRequest(manifest: UpgradeManifest, manifestHash: string): Promise<PullRequestReceipt>;
  requiredChecks(manifest: UpgradeManifest): Promise<CheckReceipt>;
  merge(manifest: UpgradeManifest, prNumber: number): Promise<MergeReceipt>;
}

export interface GitHubAppConfig {
  apiUrl: string;
  appId: number;
  privateKey: string;
  timeoutMs: number;
  mergeMethod: 'merge' | 'squash' | 'rebase';
}

const installationSchema = z.object({ id: z.number().int().positive() }).passthrough();
const tokenSchema = z
  .object({ token: z.string().min(1), expires_at: z.string().datetime({ offset: true }) })
  .passthrough();
const refSchema = z
  .object({ object: z.object({ sha: z.string().regex(/^[a-f0-9]{40}$/) }).passthrough() })
  .passthrough();
const pullSchema = z
  .object({
    number: z.number().int().positive(),
    html_url: z.string().url(),
    state: z.enum(['open', 'closed']),
    merged_at: z.string().nullable().optional(),
    merge_commit_sha: z.string().nullable().optional(),
    head: z.object({ sha: z.string().regex(/^[a-f0-9]{40}$/) }).passthrough(),
    base: z.object({ ref: z.string() }).passthrough(),
  })
  .passthrough();

interface CachedToken {
  value: string;
  expiresAt: number;
}

export class GitHubAppGateway implements GitHubGateway {
  private readonly key;
  private readonly tokens = new Map<string, CachedToken>();

  constructor(private readonly config: GitHubAppConfig) {
    this.key = createPrivateKey(config.privateKey);
    if (this.key.asymmetricKeyType !== 'rsa') {
      throw new Error('GitHub App private key must be RSA');
    }
  }

  async syncPullRequest(
    manifest: UpgradeManifest,
    manifestHash: string,
  ): Promise<PullRequestReceipt> {
    const token = await this.repositoryToken(manifest);
    await this.assertHead(manifest, token);
    const { owner, name, baseBranch, headBranch } = manifest.repository;
    const query = new URLSearchParams({
      state: 'all',
      head: `${owner}:${headBranch}`,
      base: baseBranch,
      per_page: '10',
    });
    const pulls = z
      .array(pullSchema)
      .parse(await this.request(`/repos/${encode(owner)}/${encode(name)}/pulls?${query}`, token));
    const body = `${manifest.body}\n\n---\nMizuki autonomous upgrade\n\nManifest SHA-256: \`${manifestHash}\`\nCandidate: \`${manifest.candidateSha}\``;

    let pull: z.infer<typeof pullSchema>;
    if (pulls[0]) {
      if (pulls[0].state !== 'open' || pulls[0].merged_at) {
        throw new UpdaterError(
          'pull_request_not_open',
          'Candidate branch already has a closed or merged pull request',
        );
      }
      if (pulls[0].head.sha !== manifest.candidateSha) {
        throw new UpdaterError(
          'pull_request_head_mismatch',
          'Pull request does not target candidate',
        );
      }
      pull = pullSchema.parse(
        await this.request(
          `/repos/${encode(owner)}/${encode(name)}/pulls/${pulls[0].number}`,
          token,
          { method: 'PATCH', body: { title: manifest.title, body } },
        ),
      );
    } else {
      pull = pullSchema.parse(
        await this.request(`/repos/${encode(owner)}/${encode(name)}/pulls`, token, {
          method: 'POST',
          body: { title: manifest.title, body, head: headBranch, base: baseBranch },
        }),
      );
    }
    if (pull.head.sha !== manifest.candidateSha) {
      throw new UpdaterError(
        'pull_request_head_mismatch',
        'Pull request does not target candidate',
      );
    }
    return { number: pull.number, url: pull.html_url };
  }

  async requiredChecks(manifest: UpgradeManifest): Promise<CheckReceipt> {
    const token = await this.repositoryToken(manifest);
    await this.assertHead(manifest, token);
    const { owner, name } = manifest.repository;
    const base = `/repos/${encode(owner)}/${encode(name)}/commits/${manifest.candidateSha}`;
    const [runsRaw, statusesRaw] = await Promise.all([
      this.request(`${base}/check-runs?filter=latest&per_page=100`, token, {
        headers: { accept: 'application/vnd.github+json' },
      }),
      this.request(`${base}/status?per_page=100`, token),
    ]);
    const runs = z
      .object({
        check_runs: z
          .array(
            z
              .object({
                id: z.number(),
                name: z.string(),
                status: z.enum(['queued', 'in_progress', 'completed']),
                conclusion: z.string().nullable(),
              })
              .passthrough(),
          )
          .max(100),
      })
      .passthrough()
      .parse(runsRaw).check_runs;
    const statuses = z
      .object({
        statuses: z
          .array(
            z
              .object({
                id: z.number(),
                context: z.string(),
                state: z.enum(['error', 'failure', 'pending', 'success']),
              })
              .passthrough(),
          )
          .max(100),
      })
      .passthrough()
      .parse(statusesRaw).statuses;

    const runResults = new Map<string, { id: number; status: string }>();
    for (const run of runs) {
      const status = run.status === 'completed' ? (run.conclusion ?? 'unknown') : 'pending';
      const existing = runResults.get(run.name);
      if (!existing || run.id > existing.id) runResults.set(run.name, { id: run.id, status });
    }
    const statusResults = new Map<string, { id: number; status: string }>();
    for (const status of statuses) {
      const existing = statusResults.get(status.context);
      if (!existing || status.id > existing.id) {
        statusResults.set(status.context, { id: status.id, status: status.state });
      }
    }

    const checks: Record<string, string> = {};
    let hasPending = false;
    let hasFailure = false;
    for (const required of manifest.requiredChecks) {
      const reported = [
        runResults.get(required)?.status,
        statusResults.get(required)?.status,
      ].filter((status): status is string => status !== undefined);
      if (reported.length === 0) {
        checks[required] = 'missing';
        hasPending = true;
      } else if (reported.some((status) => status !== 'success' && status !== 'pending')) {
        checks[required] = 'failure';
        hasFailure = true;
      } else if (reported.some((status) => status === 'pending')) {
        checks[required] = 'pending';
        hasPending = true;
      } else {
        checks[required] = 'success';
      }
    }
    return { status: hasFailure ? 'failed' : hasPending ? 'pending' : 'passed', checks };
  }

  async merge(manifest: UpgradeManifest, prNumber: number): Promise<MergeReceipt> {
    const token = await this.repositoryToken(manifest);
    await this.assertHead(manifest, token);
    const { owner, name, baseBranch } = manifest.repository;
    const path = `/repos/${encode(owner)}/${encode(name)}/pulls/${prNumber}`;
    const pull = pullSchema.parse(await this.request(path, token));
    if (pull.head.sha !== manifest.candidateSha || pull.base.ref !== baseBranch) {
      throw new UpdaterError('pull_request_changed', 'Pull request target changed before merge');
    }
    if (pull.merged_at) {
      if (!pull.merge_commit_sha) {
        throw new UpdaterError('merge_receipt_missing', 'Merged pull request lacks merge commit');
      }
      return { mergeSha: pull.merge_commit_sha };
    }
    if (pull.state !== 'open') {
      throw new UpdaterError('pull_request_closed', 'Pull request was closed without merging');
    }

    const result = z
      .object({
        sha: z.string(),
        merged: z.boolean(),
        message: z.string(),
      })
      .passthrough()
      .parse(
        await this.request(`${path}/merge`, token, {
          method: 'PUT',
          body: { sha: manifest.candidateSha, merge_method: this.config.mergeMethod },
        }),
      );
    if (!result.merged || !/^[a-f0-9]{40}$/.test(result.sha)) {
      throw new UpdaterError('merge_rejected', result.message || 'GitHub rejected the merge');
    }
    return { mergeSha: result.sha };
  }

  private async assertHead(manifest: UpgradeManifest, token: string): Promise<void> {
    const { owner, name, headBranch } = manifest.repository;
    const ref = refSchema.parse(
      await this.request(
        `/repos/${encode(owner)}/${encode(name)}/git/ref/heads/${encodePath(headBranch)}`,
        token,
      ),
    );
    if (ref.object.sha !== manifest.candidateSha) {
      throw new UpdaterError('branch_head_mismatch', 'Candidate branch no longer matches manifest');
    }
  }

  private async repositoryToken(manifest: UpgradeManifest): Promise<string> {
    const jwt = this.appJwt();
    const { owner, name } = manifest.repository;
    const installation = installationSchema.parse(
      await this.request(`/repos/${encode(owner)}/${encode(name)}/installation`, jwt),
    );
    const cacheKey = `${installation.id}:${owner.toLowerCase()}/${name.toLowerCase()}`;
    const cached = this.tokens.get(cacheKey);
    if (cached && cached.expiresAt > Date.now() + 60_000) return cached.value;
    const token = tokenSchema.parse(
      await this.request(`/app/installations/${installation.id}/access_tokens`, jwt, {
        method: 'POST',
        body: { repositories: [name] },
      }),
    );
    this.tokens.set(cacheKey, {
      value: token.token,
      expiresAt: new Date(token.expires_at).getTime(),
    });
    return token.token;
  }

  private appJwt(): string {
    const now = Math.floor(Date.now() / 1_000);
    const header = base64Url(JSON.stringify({ alg: 'RS256', typ: 'JWT' }));
    const payload = base64Url(
      JSON.stringify({ iat: now - 60, exp: now + 9 * 60, iss: this.config.appId }),
    );
    const signingInput = `${header}.${payload}`;
    const signer = createSign('RSA-SHA256');
    signer.update(signingInput);
    return `${signingInput}.${signer.sign(this.key).toString('base64url')}`;
  }

  private async request(
    path: string,
    token: string,
    options: {
      method?: string;
      body?: Record<string, unknown>;
      headers?: Record<string, string>;
    } = {},
  ): Promise<unknown> {
    let response: Response;
    try {
      response = await fetch(`${this.config.apiUrl}${path}`, {
        method: options.method ?? 'GET',
        headers: {
          accept: 'application/vnd.github+json',
          authorization: `Bearer ${token}`,
          'x-github-api-version': '2022-11-28',
          ...(options.body ? { 'content-type': 'application/json' } : {}),
          ...options.headers,
        },
        body: options.body ? JSON.stringify(options.body) : undefined,
        signal: AbortSignal.timeout(this.config.timeoutMs),
      });
    } catch (error) {
      throw new UpdaterError(
        'github_unavailable',
        error instanceof Error ? error.message : 'GitHub request failed',
        503,
        true,
      );
    }

    const text = await response.text();
    let payload: unknown = null;
    if (text) {
      try {
        payload = JSON.parse(text);
      } catch {
        throw new UpdaterError(
          'github_invalid_response',
          'GitHub returned invalid JSON',
          502,
          true,
        );
      }
    }
    if (!response.ok) {
      const message = readMessage(payload) ?? `GitHub returned ${response.status}`;
      const retryable =
        response.status === 408 ||
        response.status === 429 ||
        response.status >= 500 ||
        (response.status === 403 && response.headers.get('x-ratelimit-remaining') === '0');
      throw new UpdaterError('github_request_failed', message, retryable ? 503 : 422, retryable);
    }
    return payload;
  }
}

function encode(value: string): string {
  return encodeURIComponent(value);
}

function encodePath(value: string): string {
  return value.split('/').map(encode).join('/');
}

function base64Url(value: string): string {
  return Buffer.from(value).toString('base64url');
}

function readMessage(value: unknown): string | null {
  if (!value || typeof value !== 'object' || !('message' in value)) return null;
  return typeof value.message === 'string' ? value.message.slice(0, 500) : null;
}
