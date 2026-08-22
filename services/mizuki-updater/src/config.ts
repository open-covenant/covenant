import { z } from 'zod';

const envSchema = z
  .object({
    NODE_ENV: z.enum(['development', 'test', 'production']).default('development'),
    MIZUKI_UPDATER_HOST: z.string().default('127.0.0.1'),
    MIZUKI_UPDATER_PORT: z.coerce.number().int().min(1).max(65_535).default(8793),
    MIZUKI_UPDATER_AUTH_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_READ_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_DATABASE_URL: z.string().url().optional(),
    MIZUKI_UPDATER_MEMORY_STORE: z
      .enum(['true', 'false'])
      .default('false')
      .transform((value) => value === 'true'),
    MIZUKI_UPDATER_PROPOSAL_KEYS_JSON: z.string().min(2),
    MIZUKI_UPDATER_BENCHMARK_KEYS_JSON: z.string().min(2),
    MIZUKI_UPDATER_REVIEW_KEYS_JSON: z.string().min(2),
    MIZUKI_UPDATER_ALLOWED_REPOSITORIES: z.string().min(3),
    MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES: z.string().min(1),
    MIZUKI_UPDATER_HEAD_BRANCH_PREFIX: z.string().min(1).max(100),
    MIZUKI_UPDATER_MANDATORY_CHECKS: z.string().min(1),
    MIZUKI_UPDATER_PROPOSAL_MAX_AGE_MS: z.coerce
      .number()
      .int()
      .min(60_000)
      .max(30 * 24 * 60 * 60_000)
      .default(7 * 24 * 60 * 60_000),
    MIZUKI_UPDATER_ARTIFACT_ORIGINS: z.string().min(8),
    MIZUKI_UPDATER_ARTIFACT_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(120_000)
      .default(30_000),
    MIZUKI_UPDATER_ARTIFACT_MAX_BYTES: z.coerce
      .number()
      .int()
      .min(1_024)
      .max(100 * 1024 * 1024)
      .default(25 * 1024 * 1024),
    MIZUKI_UPDATER_GITHUB_API_URL: z.string().url().default('https://api.github.com'),
    MIZUKI_UPDATER_GITHUB_APP_ID: z.coerce.number().int().positive(),
    MIZUKI_UPDATER_GITHUB_PRIVATE_KEY: z.string().min(64),
    MIZUKI_UPDATER_GITHUB_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(120_000)
      .default(20_000),
    MIZUKI_UPDATER_GITHUB_MERGE_METHOD: z.enum(['merge', 'squash', 'rebase']).default('squash'),
    MIZUKI_UPDATER_SHADOW_HOOK_URL: z.string().url(),
    MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE: z.string().url(),
    MIZUKI_UPDATER_PROMOTE_HOOK_URL: z.string().url(),
    MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE: z.string().url(),
    MIZUKI_UPDATER_ROLLBACK_HOOK_URL: z.string().url(),
    MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_HOOK_TIMEOUT_MS: z.coerce.number().int().min(1_000).max(120_000).default(20_000),
    MIZUKI_UPDATER_CHECK_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(10_000)
      .max(2 * 60 * 60_000)
      .default(30 * 60_000),
    MIZUKI_UPDATER_HEALTH_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(10_000)
      .max(60 * 60_000)
      .default(10 * 60_000),
    MIZUKI_UPDATER_PROMOTION_SOAK_MS: z.coerce
      .number()
      .int()
      .min(10_000)
      .max(30 * 60_000)
      .default(2 * 60_000),
    MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(20_000)
      .max(2 * 60 * 60_000)
      .default(10 * 60_000),
    MIZUKI_UPDATER_POLL_INTERVAL_MS: z.coerce.number().int().min(250).max(60_000).default(5_000),
    MIZUKI_UPDATER_LEASE_MS: z.coerce.number().int().min(5_000).max(300_000).default(60_000),
    MIZUKI_UPDATER_MAX_ATTEMPTS: z.coerce.number().int().min(1).max(20).default(5),
  })
  .passthrough();

export interface UpdaterConfig {
  environment: 'development' | 'test' | 'production';
  host: string;
  port: number;
  authToken: string;
  readToken: string;
  databaseUrl?: string;
  memoryStore: boolean;
  trustedProposalKeys: Record<string, string>;
  trustedBenchmarkKeys: Record<string, string>;
  trustedReviewKeys: Record<string, string>;
  allowedRepositories: Set<string>;
  allowedBaseBranches: Set<string>;
  headBranchPrefix: string;
  mandatoryChecks: Set<string>;
  proposalMaxAgeMs: number;
  artifactOrigins: Set<string>;
  artifactTimeoutMs: number;
  artifactMaxBytes: number;
  githubApiUrl: string;
  githubAppId: number;
  githubPrivateKey: string;
  githubTimeoutMs: number;
  githubMergeMethod: 'merge' | 'squash' | 'rebase';
  shadowHookUrl: string;
  shadowHealthUrlTemplate: string;
  promoteHookUrl: string;
  promotionHealthUrlTemplate: string;
  rollbackHookUrl: string;
  deployHookToken: string;
  hookTimeoutMs: number;
  checkTimeoutMs: number;
  healthTimeoutMs: number;
  promotionSoakMs: number;
  promotionTimeoutMs: number;
  pollIntervalMs: number;
  leaseMs: number;
  maxAttempts: number;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): UpdaterConfig {
  const parsed = envSchema.parse(env);
  if (parsed.MIZUKI_UPDATER_MEMORY_STORE) {
    if (parsed.NODE_ENV === 'production') throw new Error('Memory store is disabled in production');
    if (!isLoopback(parsed.MIZUKI_UPDATER_HOST)) {
      throw new Error('Memory store must bind to a loopback address');
    }
  } else if (!parsed.MIZUKI_UPDATER_DATABASE_URL) {
    throw new Error('MIZUKI_UPDATER_DATABASE_URL is required');
  }

  const trustedProposalKeys = parseTrustedKeys(parsed.MIZUKI_UPDATER_PROPOSAL_KEYS_JSON);
  const trustedBenchmarkKeys = parseTrustedKeys(parsed.MIZUKI_UPDATER_BENCHMARK_KEYS_JSON);
  const trustedReviewKeys = parseTrustedKeys(parsed.MIZUKI_UPDATER_REVIEW_KEYS_JSON);
  const allowedRepositories = csvSet(parsed.MIZUKI_UPDATER_ALLOWED_REPOSITORIES, (value) =>
    value.toLowerCase(),
  );
  for (const repository of allowedRepositories) {
    if (!/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/.test(repository)) {
      throw new Error(`Invalid allowed repository: ${repository}`);
    }
  }
  const allowedBaseBranches = csvSet(parsed.MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES, (value) => value);
  for (const baseBranch of allowedBaseBranches) {
    if (!isBranchLike(baseBranch)) throw new Error(`Invalid allowed base branch: ${baseBranch}`);
  }
  const headBranchPrefix = parsed.MIZUKI_UPDATER_HEAD_BRANCH_PREFIX;
  if (
    !isBranchLike(`${headBranchPrefix}candidate`) ||
    headBranchPrefix.startsWith('/') ||
    !headBranchPrefix.endsWith('/')
  ) {
    throw new Error('Head branch prefix must be a valid branch prefix ending in /');
  }
  const mandatoryChecks = csvSet(parsed.MIZUKI_UPDATER_MANDATORY_CHECKS, (value) => value);

  const artifactOrigins = csvSet(parsed.MIZUKI_UPDATER_ARTIFACT_ORIGINS, (value) => {
    const url = new URL(value);
    if (url.protocol !== 'https:') throw new Error('Artifact origins must use HTTPS');
    if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
      throw new Error('Artifact origins must not contain credentials, a path, query, or fragment');
    }
    return url.origin;
  });

  const hookUrls = [
    parsed.MIZUKI_UPDATER_SHADOW_HOOK_URL,
    parsed.MIZUKI_UPDATER_PROMOTE_HOOK_URL,
    parsed.MIZUKI_UPDATER_ROLLBACK_HOOK_URL,
    parsed.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE.replace('{deploymentId}', 'probe'),
    parsed.MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE.replace('{deploymentId}', 'probe'),
  ];
  if (!parsed.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE.includes('{deploymentId}')) {
    throw new Error('Shadow health URL template must contain {deploymentId}');
  }
  if (!parsed.MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE.includes('{deploymentId}')) {
    throw new Error('Promotion health URL template must contain {deploymentId}');
  }
  if (
    parsed.NODE_ENV === 'production' &&
    hookUrls.some((value) => new URL(value).protocol !== 'https:')
  ) {
    throw new Error('Deployment endpoints must use HTTPS in production');
  }
  if (
    new Set([
      parsed.MIZUKI_UPDATER_AUTH_TOKEN,
      parsed.MIZUKI_UPDATER_READ_TOKEN,
      parsed.MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN,
    ]).size !== 3
  ) {
    throw new Error('Submission, read, and deployment tokens must be distinct');
  }
  if (
    parsed.MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS <
    parsed.MIZUKI_UPDATER_PROMOTION_SOAK_MS + parsed.MIZUKI_UPDATER_POLL_INTERVAL_MS
  ) {
    throw new Error('Promotion timeout must exceed the soak by at least one poll interval');
  }
  assertFixedHealthOrigin(
    parsed.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE,
    parsed.MIZUKI_UPDATER_SHADOW_HOOK_URL,
    'Shadow',
  );
  assertFixedHealthOrigin(
    parsed.MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE,
    parsed.MIZUKI_UPDATER_SHADOW_HOOK_URL,
    'Promotion',
  );
  if (
    healthPath(parsed.MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE) ===
    healthPath(parsed.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE)
  ) {
    throw new Error('Shadow and promotion health URL paths must differ');
  }
  assertDeploymentOrigins(hookUrls);
  const githubApi = new URL(parsed.MIZUKI_UPDATER_GITHUB_API_URL);
  if (githubApi.username || githubApi.password) {
    throw new Error('GitHub API URL must not include credentials');
  }
  if (parsed.NODE_ENV === 'production' && githubApi.protocol !== 'https:') {
    throw new Error('GitHub API URL must use HTTPS in production');
  }

  return {
    environment: parsed.NODE_ENV,
    host: parsed.MIZUKI_UPDATER_HOST,
    port: parsed.MIZUKI_UPDATER_PORT,
    authToken: parsed.MIZUKI_UPDATER_AUTH_TOKEN,
    readToken: parsed.MIZUKI_UPDATER_READ_TOKEN,
    databaseUrl: parsed.MIZUKI_UPDATER_DATABASE_URL,
    memoryStore: parsed.MIZUKI_UPDATER_MEMORY_STORE,
    trustedProposalKeys,
    trustedBenchmarkKeys,
    trustedReviewKeys,
    allowedRepositories,
    allowedBaseBranches,
    headBranchPrefix,
    mandatoryChecks,
    proposalMaxAgeMs: parsed.MIZUKI_UPDATER_PROPOSAL_MAX_AGE_MS,
    artifactOrigins,
    artifactTimeoutMs: parsed.MIZUKI_UPDATER_ARTIFACT_TIMEOUT_MS,
    artifactMaxBytes: parsed.MIZUKI_UPDATER_ARTIFACT_MAX_BYTES,
    githubApiUrl: parsed.MIZUKI_UPDATER_GITHUB_API_URL.replace(/\/$/, ''),
    githubAppId: parsed.MIZUKI_UPDATER_GITHUB_APP_ID,
    githubPrivateKey: parsed.MIZUKI_UPDATER_GITHUB_PRIVATE_KEY.replace(/\\n/g, '\n'),
    githubTimeoutMs: parsed.MIZUKI_UPDATER_GITHUB_TIMEOUT_MS,
    githubMergeMethod: parsed.MIZUKI_UPDATER_GITHUB_MERGE_METHOD,
    shadowHookUrl: parsed.MIZUKI_UPDATER_SHADOW_HOOK_URL,
    shadowHealthUrlTemplate: parsed.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE,
    promoteHookUrl: parsed.MIZUKI_UPDATER_PROMOTE_HOOK_URL,
    promotionHealthUrlTemplate: parsed.MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE,
    rollbackHookUrl: parsed.MIZUKI_UPDATER_ROLLBACK_HOOK_URL,
    deployHookToken: parsed.MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN,
    hookTimeoutMs: parsed.MIZUKI_UPDATER_HOOK_TIMEOUT_MS,
    checkTimeoutMs: parsed.MIZUKI_UPDATER_CHECK_TIMEOUT_MS,
    healthTimeoutMs: parsed.MIZUKI_UPDATER_HEALTH_TIMEOUT_MS,
    promotionSoakMs: parsed.MIZUKI_UPDATER_PROMOTION_SOAK_MS,
    promotionTimeoutMs: parsed.MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS,
    pollIntervalMs: parsed.MIZUKI_UPDATER_POLL_INTERVAL_MS,
    leaseMs: parsed.MIZUKI_UPDATER_LEASE_MS,
    maxAttempts: parsed.MIZUKI_UPDATER_MAX_ATTEMPTS,
  };
}

function parseTrustedKeys(value: string): Record<string, string> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new Error('Proposal keys must be valid JSON');
  }
  const keys = z.record(z.string().min(1).max(128), z.string().min(64)).parse(parsed);
  if (Object.keys(keys).length === 0) throw new Error('At least one proposal key is required');
  return keys;
}

function csvSet(value: string, normalize: (value: string) => string): Set<string> {
  const items = value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
    .map(normalize);
  if (items.length === 0) throw new Error('Configuration list must not be empty');
  return new Set(items);
}

function isLoopback(host: string): boolean {
  return host === '127.0.0.1' || host === '::1' || host === 'localhost';
}

function isBranchLike(value: string): boolean {
  return (
    /^[A-Za-z0-9._/-]+$/.test(value) &&
    !value.startsWith('/') &&
    !value.endsWith('/') &&
    !value.endsWith('.') &&
    !value.includes('..')
  );
}

function assertFixedHealthOrigin(template: string, shadowUrl: string, label: string): void {
  if (template.split('{deploymentId}').length !== 2) {
    throw new Error(`${label} health URL template must contain one {deploymentId} placeholder`);
  }
  const marker = 'mizuki-deployment-id';
  const url = new URL(template.replace('{deploymentId}', marker));
  if (!url.pathname.includes(marker) || url.search.includes(marker) || url.hash.includes(marker)) {
    throw new Error(`${label} health deployment ID must appear only in the URL path`);
  }
  if (url.origin !== new URL(shadowUrl).origin) {
    throw new Error(`${label} health and deployment hooks must use the same origin`);
  }
}

function healthPath(template: string): string {
  return new URL(template.replace('{deploymentId}', 'mizuki-deployment-id')).pathname;
}

function assertDeploymentOrigins(values: string[]): void {
  const urls = values.map((value) => new URL(value));
  if (urls.some((url) => url.username || url.password)) {
    throw new Error('Deployment endpoints must not include credentials');
  }
  if (urls.some((url) => url.origin !== urls[0].origin)) {
    throw new Error('Deployment endpoints must use one origin');
  }
}
