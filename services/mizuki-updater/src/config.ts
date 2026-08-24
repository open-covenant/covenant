import { z } from 'zod';

const emptyAsUndefined = (value: unknown): unknown =>
  typeof value === 'string' && value.trim() === '' ? undefined : value;
const optionalString = (minimum: number) =>
  z.preprocess(emptyAsUndefined, z.string().min(minimum).optional());
const optionalPositiveInteger = z.preprocess(
  emptyAsUndefined,
  z.coerce.number().int().positive().optional(),
);

const envSchema = z
  .object({
    NODE_ENV: z.enum(['development', 'test', 'production']).default('development'),
    MIZUKI_UPDATER_HOST: z.string().default('127.0.0.1'),
    MIZUKI_UPDATER_PORT: z.coerce.number().int().min(1).max(65_535).default(8793),
    MIZUKI_UPDATER_SUBMIT_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_CONTROL_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_READ_TOKEN: z.string().min(32),
    MIZUKI_UPDATER_DATABASE_URL: z.string().url().optional(),
    MIZUKI_UPDATER_MEMORY_STORE: z
      .enum(['true', 'false'])
      .default('false')
      .transform((value) => value === 'true'),
    MIZUKI_UPDATER_PROPOSAL_KEYS_JSON: optionalString(2),
    MIZUKI_UPDATER_BENCHMARK_KEYS_JSON: optionalString(2),
    MIZUKI_UPDATER_REVIEW_KEYS_JSON: optionalString(2),
    MIZUKI_UPDATER_ALLOWED_REPOSITORIES: z.string().min(3),
    MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES: z.string().min(1),
    MIZUKI_UPDATER_HEAD_BRANCH_PREFIX: z.string().min(1).max(100),
    MIZUKI_UPDATER_MANDATORY_CHECKS: z.string().min(1),
    MIZUKI_UPDATER_CHECK_PRODUCERS_JSON: optionalString(2),
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
    MIZUKI_UPDATER_GITHUB_APP_ID: optionalPositiveInteger,
    MIZUKI_UPDATER_GITHUB_PRIVATE_KEY: optionalString(64),
    MIZUKI_UPDATER_GITHUB_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(120_000)
      .default(20_000),
    MIZUKI_UPDATER_GITHUB_MERGE_METHOD: z.enum(['merge', 'squash', 'rebase']).default('squash'),
    MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: optionalString(3),
    MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: optionalString(32),
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
      .max(24 * 60 * 60_000)
      .default(10_000),
    MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(20_000)
      .max(24 * 60 * 60_000)
      .default(120_000),
    MIZUKI_UPDATER_POLL_INTERVAL_MS: z.coerce.number().int().min(250).max(60_000).default(5_000),
    MIZUKI_UPDATER_LEASE_MS: z.coerce.number().int().min(5_000).max(300_000).default(60_000),
    MIZUKI_UPDATER_MAX_ATTEMPTS: z.coerce.number().int().min(1).max(20).default(5),
  })
  .passthrough();

type ParsedEnv = z.infer<typeof envSchema>;

const operationalValues = [
  'MIZUKI_UPDATER_PROPOSAL_KEYS_JSON',
  'MIZUKI_UPDATER_BENCHMARK_KEYS_JSON',
  'MIZUKI_UPDATER_REVIEW_KEYS_JSON',
  'MIZUKI_UPDATER_CHECK_PRODUCERS_JSON',
  'MIZUKI_UPDATER_GITHUB_APP_ID',
  'MIZUKI_UPDATER_GITHUB_PRIVATE_KEY',
  'MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT',
  'MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN',
] as const;

export interface UpdaterOperationalConfig {
  trustedProposalKeys: Record<string, string>;
  trustedBenchmarkKeys: Record<string, string>;
  trustedReviewKeys: Record<string, string>;
  checkProducers: Map<string, CheckProducerPolicy>;
  githubAppId: number;
  githubPrivateKey: string;
  shadowHookUrl: string;
  shadowHealthUrlTemplate: string;
  promoteHookUrl: string;
  promotionHealthUrlTemplate: string;
  finalizeHookUrl: string;
  rollbackHookUrl: string;
  deployReadinessUrl: string;
  deployHookToken: string;
}

export interface CheckProducerPolicy {
  checkRunAppId: number;
  workflowId: number;
  workflowPath: string;
  event: 'pull_request';
  headBranch: 'manifest';
  headSha: 'candidate';
  baseBranch: 'manifest';
  baseSha: 'signed';
  definitionRef: 'base';
}

export interface UpdaterConfig {
  environment: 'development' | 'test' | 'production';
  host: string;
  port: number;
  submitToken: string;
  controlToken: string;
  readToken: string;
  databaseUrl?: string;
  memoryStore: boolean;
  operational?: UpdaterOperationalConfig;
  operationalFailures: string[];
  allowedRepositories: Set<string>;
  allowedBaseBranches: Set<string>;
  headBranchPrefix: string;
  mandatoryChecks: Set<string>;
  proposalMaxAgeMs: number;
  artifactOrigins: Set<string>;
  artifactTimeoutMs: number;
  artifactMaxBytes: number;
  githubApiUrl: string;
  githubTimeoutMs: number;
  githubMergeMethod: 'merge' | 'squash' | 'rebase';
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

  const credentials = [
    parsed.MIZUKI_UPDATER_SUBMIT_TOKEN,
    parsed.MIZUKI_UPDATER_CONTROL_TOKEN,
    parsed.MIZUKI_UPDATER_READ_TOKEN,
    ...(parsed.MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN ? [parsed.MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN] : []),
  ];
  if (new Set(credentials).size !== credentials.length) {
    throw new Error('Submission, control, read, and deployment tokens must be distinct');
  }
  if (
    parsed.MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS <
    parsed.MIZUKI_UPDATER_PROMOTION_SOAK_MS + parsed.MIZUKI_UPDATER_POLL_INTERVAL_MS
  ) {
    throw new Error(
      'Promotion timeout must exceed the observation window by at least one poll interval',
    );
  }
  const minimumLeaseMs =
    Math.max(
      parsed.MIZUKI_UPDATER_HOOK_TIMEOUT_MS,
      parsed.MIZUKI_UPDATER_GITHUB_TIMEOUT_MS,
      parsed.MIZUKI_UPDATER_ARTIFACT_TIMEOUT_MS,
    ) +
    2 * parsed.MIZUKI_UPDATER_POLL_INTERVAL_MS;
  if (parsed.MIZUKI_UPDATER_LEASE_MS < minimumLeaseMs) {
    throw new Error(
      `Upgrade lease must be at least ${minimumLeaseMs}ms for the configured external timeouts`,
    );
  }
  const githubApi = new URL(parsed.MIZUKI_UPDATER_GITHUB_API_URL);
  if (githubApi.username || githubApi.password) {
    throw new Error('GitHub API URL must not include credentials');
  }
  if (parsed.NODE_ENV === 'production' && githubApi.protocol !== 'https:') {
    throw new Error('GitHub API URL must use HTTPS in production');
  }

  const operationalFailures = missingOperationalValues(parsed);
  const operational =
    operationalFailures.length === 0
      ? operationalConfig(parsed, parsed.NODE_ENV === 'production')
      : undefined;
  if (operational) {
    for (const check of mandatoryChecks) {
      if (!operational.checkProducers.has(check)) {
        throw new Error(`Mandatory check lacks a pinned producer: ${check}`);
      }
    }
  }

  return {
    environment: parsed.NODE_ENV,
    host: parsed.MIZUKI_UPDATER_HOST,
    port: parsed.MIZUKI_UPDATER_PORT,
    submitToken: parsed.MIZUKI_UPDATER_SUBMIT_TOKEN,
    controlToken: parsed.MIZUKI_UPDATER_CONTROL_TOKEN,
    readToken: parsed.MIZUKI_UPDATER_READ_TOKEN,
    databaseUrl: parsed.MIZUKI_UPDATER_DATABASE_URL,
    memoryStore: parsed.MIZUKI_UPDATER_MEMORY_STORE,
    operational,
    operationalFailures,
    allowedRepositories,
    allowedBaseBranches,
    headBranchPrefix,
    mandatoryChecks,
    proposalMaxAgeMs: parsed.MIZUKI_UPDATER_PROPOSAL_MAX_AGE_MS,
    artifactOrigins,
    artifactTimeoutMs: parsed.MIZUKI_UPDATER_ARTIFACT_TIMEOUT_MS,
    artifactMaxBytes: parsed.MIZUKI_UPDATER_ARTIFACT_MAX_BYTES,
    githubApiUrl: parsed.MIZUKI_UPDATER_GITHUB_API_URL.replace(/\/$/, ''),
    githubTimeoutMs: parsed.MIZUKI_UPDATER_GITHUB_TIMEOUT_MS,
    githubMergeMethod: parsed.MIZUKI_UPDATER_GITHUB_MERGE_METHOD,
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

function missingOperationalValues(parsed: ParsedEnv): string[] {
  return operationalValues.filter((name) => parsed[name] === undefined);
}

function operationalConfig(parsed: ParsedEnv, production: boolean): UpdaterOperationalConfig {
  const origin = deploymentControllerOrigin(
    parsed.MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT!,
    production,
  );

  return {
    trustedProposalKeys: parseTrustedKeys(parsed.MIZUKI_UPDATER_PROPOSAL_KEYS_JSON!),
    trustedBenchmarkKeys: parseTrustedKeys(parsed.MIZUKI_UPDATER_BENCHMARK_KEYS_JSON!),
    trustedReviewKeys: parseTrustedKeys(parsed.MIZUKI_UPDATER_REVIEW_KEYS_JSON!),
    checkProducers: parseCheckProducers(parsed.MIZUKI_UPDATER_CHECK_PRODUCERS_JSON!),
    githubAppId: parsed.MIZUKI_UPDATER_GITHUB_APP_ID!,
    githubPrivateKey: parsed.MIZUKI_UPDATER_GITHUB_PRIVATE_KEY!.replace(/\\n/g, '\n'),
    shadowHookUrl: `${origin}/v1/deployments/shadow`,
    shadowHealthUrlTemplate: `${origin}/v1/deployments/shadow/{deploymentId}/health`,
    promoteHookUrl: `${origin}/v1/deployments/promote`,
    promotionHealthUrlTemplate: `${origin}/v1/deployments/production/{deploymentId}/health`,
    finalizeHookUrl: `${origin}/v1/deployments/finalize`,
    rollbackHookUrl: `${origin}/v1/deployments/rollback`,
    deployReadinessUrl: `${origin}/readyz`,
    deployHookToken: parsed.MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN!,
  };
}

const producerPolicySchema = z
  .object({
    checkRunAppId: z.number().int().positive().safe(),
    workflowId: z.number().int().positive().safe(),
    workflowPath: z
      .string()
      .regex(/^\.github\/workflows\/[A-Za-z0-9_.-]+\.ya?ml$/)
      .max(200),
    event: z.literal('pull_request'),
    headBranch: z.literal('manifest'),
    headSha: z.literal('candidate'),
    baseBranch: z.literal('manifest'),
    baseSha: z.literal('signed'),
    definitionRef: z.literal('base'),
  })
  .strict();

function parseCheckProducers(value: string): Map<string, CheckProducerPolicy> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new Error('MIZUKI_UPDATER_CHECK_PRODUCERS_JSON must be valid JSON');
  }
  const record = z
    .record(z.string().min(1).max(200), producerPolicySchema)
    .refine(
      (policies) => Object.keys(policies).length > 0,
      'At least one check producer is required',
    )
    .parse(parsed);
  return new Map(Object.entries(record));
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

function deploymentControllerOrigin(value: string, production: boolean): string {
  const raw = value.trim();
  const url = new URL(raw.includes('://') ? raw : `http://${raw}`);
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('Deployment controller must be a credential-free host and port');
  }
  if (!['http:', 'https:'].includes(url.protocol)) {
    throw new Error('Deployment controller protocol is invalid');
  }
  if (production) {
    if (
      url.protocol !== 'http:' ||
      url.hostname !== 'mizuki-deployment-controller' ||
      url.port !== '8794'
    ) {
      throw new Error(
        'Production deployment controller must use the fixed Render private origin mizuki-deployment-controller:8794',
      );
    }
  }
  return url.origin;
}
