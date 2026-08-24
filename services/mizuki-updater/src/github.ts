import { createPrivateKey, createSign } from 'node:crypto';
import { z } from 'zod';
import type { CheckProducerPolicy } from './config.js';
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

export type MergeState = { status: 'open' } | ({ status: 'merged' } & MergeReceipt);

export interface GitHubGateway {
  syncPullRequest(manifest: UpgradeManifest, manifestHash: string): Promise<PullRequestReceipt>;
  requiredChecks(manifest: UpgradeManifest, prNumber: number): Promise<CheckReceipt>;
  mergeState(manifest: UpgradeManifest, prNumber: number): Promise<MergeState>;
  merge(manifest: UpgradeManifest, prNumber: number): Promise<MergeReceipt>;
}

export interface GitHubAppConfig {
  apiUrl: string;
  appId: number;
  privateKey: string;
  repositories: ReadonlySet<string>;
  timeoutMs: number;
  mergeMethod: 'merge' | 'squash' | 'rebase';
  checkProducers: ReadonlyMap<string, CheckProducerPolicy>;
}

const TOKEN_REFRESH_WINDOW_MS = 5 * 60_000;
const TOKEN_MAX_TTL_MS = 65 * 60_000;
const TOKEN_CACHE_LIMIT = 64;
const UPDATER_PERMISSIONS = {
  actions: 'read',
  checks: 'read',
  contents: 'write',
  metadata: 'read',
  pull_requests: 'write',
} as const;
const permissionSchema = z.record(z.string(), z.enum(['read', 'write']));
const installationSchema = z
  .object({
    id: z.number().int().positive(),
    app_id: z.number().int().positive(),
    repository_selection: z.literal('selected'),
    permissions: permissionSchema,
    suspended_at: z.string().datetime({ offset: true }).nullable().optional(),
  })
  .passthrough();
const appSchema = z
  .object({ id: z.number().int().positive(), permissions: permissionSchema })
  .passthrough();
const tokenSchema = z
  .object({
    token: z.string().min(20).max(4_096).regex(/^\S+$/),
    expires_at: z.string().datetime({ offset: true }),
    permissions: z
      .object({
        actions: z.literal('read'),
        checks: z.literal('read'),
        contents: z.literal('write'),
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
    base: z.object({ ref: z.string(), sha: z.string().regex(/^[a-f0-9]{40}$/) }).passthrough(),
  })
  .passthrough();
const checkPullSchema = z
  .object({
    number: z.number().int().positive(),
    state: z.literal('open'),
    merged_at: z.null(),
    head: z
      .object({
        ref: z.string().min(1).max(255),
        sha: z.string().regex(/^[a-f0-9]{40}$/),
        repo: z.object({ id: z.number().int().positive(), full_name: z.string() }).passthrough(),
      })
      .passthrough(),
    base: z
      .object({
        ref: z.string().min(1).max(255),
        sha: z.string().regex(/^[a-f0-9]{40}$/),
        repo: z.object({ id: z.number().int().positive(), full_name: z.string() }).passthrough(),
      })
      .passthrough(),
  })
  .passthrough();
const checkRunSchema = z
  .object({
    id: z.number().int().positive(),
    name: z.string().min(1).max(200),
    head_sha: z.string().regex(/^[a-f0-9]{40}$/),
    status: z.enum(['queued', 'in_progress', 'completed']),
    conclusion: z.string().nullable(),
    app: z.object({ id: z.number().int().positive().safe() }).passthrough(),
    check_suite: z.object({ id: z.number().int().positive().safe() }).passthrough(),
  })
  .passthrough();
const workflowSchema = z
  .object({
    id: z.number().int().positive().safe(),
    path: z.string().min(1).max(200),
    state: z.literal('active'),
  })
  .passthrough();
const contentSchema = z
  .object({
    type: z.literal('file'),
    path: z.string().min(1).max(200),
    sha: z.string().regex(/^[a-f0-9]{40}$/),
  })
  .passthrough();
const workflowRunSchema = z
  .object({
    id: z.number().int().positive().safe(),
    check_suite_id: z.number().int().positive().safe(),
    head_branch: z.string().min(1).max(255),
    head_sha: z.string().regex(/^[a-f0-9]{40}$/),
    path: z.string().min(1).max(200),
    event: z.string().min(1).max(100),
    workflow_id: z.number().int().positive().safe(),
    repository: z
      .object({ id: z.number().int().positive(), full_name: z.string().min(3).max(201) })
      .passthrough(),
    head_repository: z
      .object({ id: z.number().int().positive(), full_name: z.string().min(3).max(201) })
      .passthrough(),
    pull_requests: z
      .array(
        z
          .object({
            number: z.number().int().positive(),
            head: z
              .object({
                ref: z.string().min(1).max(255),
                sha: z.string().regex(/^[a-f0-9]{40}$/),
                repo: z.object({ id: z.number().int().positive() }).passthrough(),
              })
              .passthrough(),
            base: z
              .object({
                ref: z.string().min(1).max(255),
                sha: z.string().regex(/^[a-f0-9]{40}$/),
                repo: z.object({ id: z.number().int().positive() }).passthrough(),
              })
              .passthrough(),
          })
          .passthrough(),
      )
      .max(100),
  })
  .passthrough();

type WorkflowRun = z.infer<typeof workflowRunSchema>;

interface WorkflowEvidence {
  identityMatches: boolean;
  definitionMatches: boolean;
  runs: WorkflowRun[];
}

interface CachedToken {
  value: string;
  expiresAt: number;
}

export class GitHubAppGateway implements GitHubGateway {
  private readonly key;
  private readonly tokens = new Map<string, CachedToken>();
  private readonly tokenRequests = new Map<string, Promise<CachedToken>>();

  constructor(private readonly config: GitHubAppConfig) {
    this.key = createPrivateKey(config.privateKey);
    if (this.key.asymmetricKeyType !== 'rsa') {
      throw new Error('GitHub App private key must be RSA');
    }
  }

  async readiness(): Promise<void> {
    const app = appSchema.parse(await this.request('/app', this.appJwt()));
    if (app.id !== this.config.appId) throw new Error('GitHub authenticated a different App');
    assertExactPermissions(app.permissions, UPDATER_PERMISSIONS, 'GitHub App');
    for (const repository of this.config.repositories) {
      const [owner, name] = repositoryParts(repository);
      const token = await this.repositoryTokenFor(owner, name);
      await this.assertConfiguredWorkflows(owner, name, token.value);
    }
  }

  async syncPullRequest(
    manifest: UpgradeManifest,
    manifestHash: string,
  ): Promise<PullRequestReceipt> {
    await this.assertRefs(manifest);
    const { owner, name, baseBranch, headBranch } = manifest.repository;
    const query = new URLSearchParams({
      state: 'all',
      head: `${owner}:${headBranch}`,
      base: baseBranch,
      per_page: '10',
    });
    const pulls = z
      .array(pullSchema)
      .parse(
        await this.repositoryRequest(
          manifest,
          `/repos/${encode(owner)}/${encode(name)}/pulls?${query}`,
        ),
      );
    const body = `${manifest.body}\n\n---\nMizuki autonomous upgrade\n\nManifest SHA-256: \`${manifestHash}\`\nCandidate: \`${manifest.candidateSha}\`\nBase: \`${manifest.repository.baseSha}\``;

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
        await this.repositoryRequest(
          manifest,
          `/repos/${encode(owner)}/${encode(name)}/pulls/${pulls[0].number}`,
          { method: 'PATCH', body: { title: manifest.title, body } },
        ),
      );
    } else {
      pull = pullSchema.parse(
        await this.repositoryRequest(manifest, `/repos/${encode(owner)}/${encode(name)}/pulls`, {
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

  async requiredChecks(manifest: UpgradeManifest, prNumber: number): Promise<CheckReceipt> {
    await this.assertRefs(manifest);
    await this.assertCheckPull(manifest, prNumber);
    const { owner, name } = manifest.repository;
    const base = `/repos/${encode(owner)}/${encode(name)}/commits/${manifest.candidateSha}`;
    const runsRaw = await this.repositoryRequest(
      manifest,
      `${base}/check-runs?filter=latest&per_page=100`,
      { headers: { accept: 'application/vnd.github+json' } },
    );
    const runPage = z
      .object({
        total_count: z.number().int().nonnegative().max(100),
        check_runs: z.array(checkRunSchema).max(100),
      })
      .passthrough()
      .parse(runsRaw);
    if (runPage.total_count !== runPage.check_runs.length) {
      throw new UpdaterError(
        'github_check_page_incomplete',
        'GitHub returned an incomplete check-run page',
        502,
        true,
      );
    }

    const checks: Record<string, string> = {};
    const namedRuns = new Map<string, z.infer<typeof checkRunSchema>[]>();
    let hasPending = false;
    let hasFailure = false;
    for (const required of manifest.requiredChecks) {
      const policy = this.config.checkProducers.get(required);
      if (!policy) {
        checks[required] = 'producer-policy-missing';
        hasFailure = true;
        continue;
      }
      const matching = runPage.check_runs.filter((run) => run.name === required);
      namedRuns.set(required, matching);
      if (matching.some((run) => run.app.id !== policy.checkRunAppId)) {
        checks[required] = 'untrusted-producer';
        hasFailure = true;
        continue;
      }
      const run = latestById(matching);
      if (!run) {
        checks[required] = 'missing';
        hasPending = true;
      } else if (run.status !== 'completed') {
        checks[required] = 'pending';
        hasPending = true;
      } else if (run.conclusion !== 'success') {
        checks[required] = 'failure';
        hasFailure = true;
      } else {
        checks[required] = 'success';
      }
    }
    if (hasFailure || hasPending) {
      return { status: hasFailure ? 'failed' : 'pending', checks };
    }

    const evidence = await this.workflowEvidence(manifest);
    for (const required of manifest.requiredChecks) {
      const policy = this.config.checkProducers.get(required)!;
      const workflow = evidence.get(workflowKey(policy));
      if (!workflow?.identityMatches) {
        checks[required] = 'untrusted-workflow';
        hasFailure = true;
        continue;
      }
      if (!workflow.definitionMatches) {
        checks[required] = 'workflow-definition-changed';
        hasFailure = true;
        continue;
      }
      if (
        namedRuns
          .get(required)!
          .some(
            (run) =>
              run.head_sha !== manifest.candidateSha ||
              !workflow.runs.some(
                (workflowRun) =>
                  workflowRun.check_suite_id === run.check_suite.id &&
                  this.isTrustedWorkflowRun(workflowRun, manifest, prNumber, policy),
              ),
          )
      ) {
        checks[required] = 'untrusted-workflow';
        hasFailure = true;
      }
    }
    return { status: hasFailure ? 'failed' : 'passed', checks };
  }

  async merge(manifest: UpgradeManifest, prNumber: number): Promise<MergeReceipt> {
    const state = await this.mergeState(manifest, prNumber);
    if (state.status === 'merged') return { mergeSha: state.mergeSha };

    const { owner, name } = manifest.repository;
    const path = `/repos/${encode(owner)}/${encode(name)}/pulls/${prNumber}`;
    await this.assertRefs(manifest);
    const result = z
      .object({
        sha: z.string(),
        merged: z.boolean(),
        message: z.string(),
      })
      .passthrough()
      .parse(
        await this.repositoryRequest(manifest, `${path}/merge`, {
          method: 'PUT',
          body: { sha: manifest.candidateSha, merge_method: this.config.mergeMethod },
        }),
      );
    if (!result.merged || !/^[a-f0-9]{40}$/.test(result.sha)) {
      throw new UpdaterError('merge_rejected', result.message || 'GitHub rejected the merge');
    }
    return { mergeSha: result.sha };
  }

  async mergeState(manifest: UpgradeManifest, prNumber: number): Promise<MergeState> {
    const { owner, name, baseBranch } = manifest.repository;
    const path = `/repos/${encode(owner)}/${encode(name)}/pulls/${prNumber}`;
    const pull = pullSchema.parse(await this.repositoryRequest(manifest, path));
    if (
      pull.head.sha !== manifest.candidateSha ||
      pull.base.ref !== baseBranch ||
      pull.base.sha !== manifest.repository.baseSha
    ) {
      throw new UpdaterError('pull_request_changed', 'Pull request target changed before merge');
    }
    if (pull.merged_at) {
      if (!pull.merge_commit_sha) {
        throw new UpdaterError('merge_receipt_missing', 'Merged pull request lacks merge commit');
      }
      return { status: 'merged', mergeSha: pull.merge_commit_sha };
    }
    if (pull.state !== 'open') {
      throw new UpdaterError('pull_request_closed', 'Pull request was closed without merging');
    }

    return { status: 'open' };
  }

  private async workflowEvidence(
    manifest: UpgradeManifest,
  ): Promise<Map<string, WorkflowEvidence>> {
    const policies = new Map<string, CheckProducerPolicy>();
    for (const check of manifest.requiredChecks) {
      const policy = this.config.checkProducers.get(check);
      if (policy) policies.set(workflowKey(policy), policy);
    }
    const evidence = await Promise.all(
      [...policies].map(async ([key, policy]) => {
        const value = await this.loadWorkflowEvidence(manifest, policy);
        return [key, value] as const;
      }),
    );
    return new Map(evidence);
  }

  private async loadWorkflowEvidence(
    manifest: UpgradeManifest,
    policy: CheckProducerPolicy,
  ): Promise<WorkflowEvidence> {
    const { owner, name, baseSha } = manifest.repository;
    const repository = `/repos/${encode(owner)}/${encode(name)}`;
    const query = new URLSearchParams({
      head_sha: manifest.candidateSha,
      event: policy.event,
      per_page: '100',
    });
    const contentPath = `${repository}/contents/${encodePath(policy.workflowPath)}`;
    const [workflowRaw, baseContentRaw, candidateContentRaw, runsRaw] = await Promise.all([
      this.repositoryRequest(manifest, `${repository}/actions/workflows/${policy.workflowId}`),
      this.repositoryRequest(manifest, `${contentPath}?ref=${encode(baseSha)}`),
      this.repositoryRequest(manifest, `${contentPath}?ref=${encode(manifest.candidateSha)}`),
      this.repositoryRequest(
        manifest,
        `${repository}/actions/workflows/${policy.workflowId}/runs?${query}`,
      ),
    ]);
    const workflow = workflowSchema.parse(workflowRaw);
    const baseContent = contentSchema.parse(baseContentRaw);
    const candidateContent = contentSchema.parse(candidateContentRaw);
    const runPage = z
      .object({
        total_count: z.number().int().nonnegative().max(100),
        workflow_runs: z.array(workflowRunSchema).max(100),
      })
      .passthrough()
      .parse(runsRaw);
    if (runPage.total_count !== runPage.workflow_runs.length) {
      throw new UpdaterError(
        'github_workflow_page_incomplete',
        'GitHub returned an incomplete workflow-run page',
        502,
        true,
      );
    }
    return {
      identityMatches:
        hasTrustedSemantics(policy) &&
        workflow.id === policy.workflowId &&
        workflow.path === policy.workflowPath,
      definitionMatches:
        baseContent.path === policy.workflowPath &&
        candidateContent.path === policy.workflowPath &&
        baseContent.sha === candidateContent.sha,
      runs: runPage.workflow_runs,
    };
  }

  private isTrustedWorkflowRun(
    run: WorkflowRun,
    manifest: UpgradeManifest,
    prNumber: number,
    policy: CheckProducerPolicy,
  ): boolean {
    const { owner, name, headBranch, baseBranch, baseSha } = manifest.repository;
    const repository = `${owner}/${name}`.toLowerCase();
    if (
      run.workflow_id !== policy.workflowId ||
      run.path !== policy.workflowPath ||
      run.event !== policy.event ||
      run.head_branch !== headBranch ||
      run.head_sha !== manifest.candidateSha ||
      run.repository.full_name.toLowerCase() !== repository ||
      run.head_repository.full_name.toLowerCase() !== repository ||
      run.head_repository.id !== run.repository.id
    ) {
      return false;
    }
    return run.pull_requests.some(
      (pull) =>
        pull.number === prNumber &&
        pull.head.ref === headBranch &&
        pull.head.sha === manifest.candidateSha &&
        pull.head.repo.id === run.head_repository.id &&
        pull.base.ref === baseBranch &&
        pull.base.sha === baseSha &&
        pull.base.repo.id === run.repository.id,
    );
  }

  private async assertCheckPull(manifest: UpgradeManifest, prNumber: number): Promise<void> {
    if (!Number.isSafeInteger(prNumber) || prNumber < 1) {
      throw new Error('Pull request number is invalid');
    }
    const { owner, name, headBranch, baseBranch, baseSha } = manifest.repository;
    const repository = `${owner}/${name}`.toLowerCase();
    const pull = checkPullSchema.parse(
      await this.repositoryRequest(
        manifest,
        `/repos/${encode(owner)}/${encode(name)}/pulls/${prNumber}`,
      ),
    );
    if (
      pull.number !== prNumber ||
      pull.head.ref !== headBranch ||
      pull.head.sha !== manifest.candidateSha ||
      pull.head.repo.full_name.toLowerCase() !== repository ||
      pull.base.ref !== baseBranch ||
      pull.base.sha !== baseSha ||
      pull.base.repo.full_name.toLowerCase() !== repository
    ) {
      throw new UpdaterError(
        'pull_request_changed',
        'Pull request target changed before check admission',
      );
    }
  }

  private async assertRefs(manifest: UpgradeManifest): Promise<void> {
    const { owner, name, headBranch, baseBranch, baseSha } = manifest.repository;
    const root = `/repos/${encode(owner)}/${encode(name)}/git/ref/heads`;
    const [head, base] = await Promise.all([
      this.repositoryRequest(manifest, `${root}/${encodePath(headBranch)}`),
      this.repositoryRequest(manifest, `${root}/${encodePath(baseBranch)}`),
    ]);
    if (refSchema.parse(head).object.sha !== manifest.candidateSha) {
      throw new UpdaterError('branch_head_mismatch', 'Candidate branch no longer matches manifest');
    }
    if (refSchema.parse(base).object.sha !== baseSha) {
      throw new UpdaterError('base_branch_changed', 'Base branch no longer matches manifest');
    }
  }

  private async assertConfiguredWorkflows(
    owner: string,
    name: string,
    token: string,
  ): Promise<void> {
    const workflows = new Map<number, string>();
    for (const policy of this.config.checkProducers.values()) {
      if (!hasTrustedSemantics(policy)) {
        throw new Error('Workflow policy does not use the trusted release semantics');
      }
      const configuredPath = workflows.get(policy.workflowId);
      if (configuredPath && configuredPath !== policy.workflowPath) {
        throw new Error('One workflow ID is configured with conflicting paths');
      }
      workflows.set(policy.workflowId, policy.workflowPath);
    }
    await Promise.all(
      [...workflows].map(async ([workflowId, workflowPath]) => {
        const workflow = workflowSchema.parse(
          await this.request(
            `/repos/${encode(owner)}/${encode(name)}/actions/workflows/${workflowId}`,
            token,
          ),
        );
        if (workflow.id !== workflowId || workflow.path !== workflowPath) {
          throw new Error('GitHub workflow identity does not match updater policy');
        }
      }),
    );
  }

  private async repositoryToken(manifest: UpgradeManifest): Promise<CachedToken> {
    const { owner, name } = manifest.repository;
    return this.repositoryTokenFor(owner, name);
  }

  private async repositoryTokenFor(owner: string, name: string): Promise<CachedToken> {
    const cacheKey = `${owner.toLowerCase()}/${name.toLowerCase()}`;
    const cached = this.tokens.get(cacheKey);
    if (cached && cached.expiresAt > Date.now() + TOKEN_REFRESH_WINDOW_MS) {
      this.tokens.delete(cacheKey);
      this.tokens.set(cacheKey, cached);
      return cached;
    }
    if (cached) this.tokens.delete(cacheKey);

    const pending = this.tokenRequests.get(cacheKey);
    if (pending) return pending;
    const request = this.mintRepositoryToken(owner, name, cacheKey).finally(() => {
      if (this.tokenRequests.get(cacheKey) === request) this.tokenRequests.delete(cacheKey);
    });
    this.tokenRequests.set(cacheKey, request);
    return request;
  }

  private async mintRepositoryToken(
    owner: string,
    name: string,
    cacheKey: string,
  ): Promise<CachedToken> {
    const jwt = this.appJwt();
    const installation = installationSchema.parse(
      await this.request(`/repos/${encode(owner)}/${encode(name)}/installation`, jwt),
    );
    if (installation.app_id !== this.config.appId) {
      throw new Error('GitHub returned an installation for a different App');
    }
    if (installation.suspended_at) throw new Error('GitHub App installation is suspended');
    assertExactPermissions(
      installation.permissions,
      UPDATER_PERMISSIONS,
      'GitHub App installation',
    );
    const token = tokenSchema.parse(
      await this.request(`/app/installations/${installation.id}/access_tokens`, jwt, {
        method: 'POST',
        body: { repositories: [name], permissions: UPDATER_PERMISSIONS },
      }),
    );
    if (
      token.repositories[0]?.name.toLowerCase() !== name.toLowerCase() ||
      token.repositories[0]?.full_name.toLowerCase() !== `${owner}/${name}`.toLowerCase()
    ) {
      throw new Error('GitHub returned an incorrectly scoped repository token');
    }
    const expiresAt = new Date(token.expires_at).getTime();
    const ttl = expiresAt - Date.now();
    if (ttl <= TOKEN_REFRESH_WINDOW_MS || ttl > TOKEN_MAX_TTL_MS) {
      throw new Error('GitHub returned an invalid repository token lifetime');
    }
    const result = {
      value: token.token,
      expiresAt,
    };
    if (!this.tokens.has(cacheKey) && this.tokens.size >= TOKEN_CACHE_LIMIT) {
      const oldest = this.tokens.keys().next().value;
      if (oldest) this.tokens.delete(oldest);
    }
    this.tokens.set(cacheKey, result);
    return result;
  }

  private invalidateToken(manifest: UpgradeManifest, token: string): void {
    const { owner, name } = manifest.repository;
    const key = `${owner.toLowerCase()}/${name.toLowerCase()}`;
    if (this.tokens.get(key)?.value === token) this.tokens.delete(key);
  }

  private async repositoryRequest(
    manifest: UpgradeManifest,
    path: string,
    options: {
      method?: string;
      body?: Record<string, unknown>;
      headers?: Record<string, string>;
    } = {},
  ): Promise<unknown> {
    const first = await this.repositoryToken(manifest);
    try {
      return await this.request(path, first.value, options);
    } catch (cause) {
      if (!(cause instanceof GitHubRequestError) || cause.status !== 401) throw cause;
    }
    this.invalidateToken(manifest, first.value);
    const replacement = await this.repositoryToken(manifest);
    return this.request(path, replacement.value, options);
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
        if (response.ok) {
          throw new UpdaterError(
            'github_invalid_response',
            'GitHub returned invalid JSON',
            502,
            true,
          );
        }
      }
    }
    if (!response.ok) {
      const message = readMessage(payload) ?? `GitHub returned ${response.status}`;
      const retryable =
        response.status === 408 ||
        response.status === 429 ||
        response.status >= 500 ||
        (response.status === 403 && response.headers.get('x-ratelimit-remaining') === '0');
      throw new GitHubRequestError(response.status, message, retryable ? 503 : 422, retryable);
    }
    return payload;
  }
}

function latestById<T extends { id: number }>(values: readonly T[]): T | undefined {
  let latest: T | undefined;
  for (const value of values) {
    if (!latest || value.id > latest.id) latest = value;
  }
  return latest;
}

function workflowKey(policy: CheckProducerPolicy): string {
  return `${policy.workflowId}:${policy.workflowPath}:${policy.event}`;
}

function hasTrustedSemantics(policy: CheckProducerPolicy): boolean {
  return (
    policy.event === 'pull_request' &&
    policy.headBranch === 'manifest' &&
    policy.headSha === 'candidate' &&
    policy.baseBranch === 'manifest' &&
    policy.baseSha === 'signed' &&
    policy.definitionRef === 'base'
  );
}

class GitHubRequestError extends UpdaterError {
  constructor(
    readonly status: number,
    message: string,
    serviceStatus: number,
    retryable: boolean,
  ) {
    super('github_request_failed', message, serviceStatus, retryable);
  }
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
  throw new Error(`${subject} permissions do not match the required updater contract`);
}

function encode(value: string): string {
  return encodeURIComponent(value);
}

function repositoryParts(repository: string): [string, string] {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository) || repository.length > 201) {
    throw new Error('Updater repository identity is invalid');
  }
  return repository.split('/') as [string, string];
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
