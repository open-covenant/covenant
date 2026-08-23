import { address } from '@solana/kit';

export type Config = ReturnType<typeof loadConfig>;

export function loadConfig(env: NodeJS.ProcessEnv = process.env) {
  const updaterUrl = optionalHttpUrl(env.MIZUKI_UPDATER_URL);
  const updaterToken = env.MIZUKI_UPDATER_TOKEN;
  if (Boolean(updaterUrl) !== Boolean(updaterToken)) {
    throw new Error('MIZUKI_UPDATER_URL and MIZUKI_UPDATER_TOKEN must be configured together');
  }
  if (updaterToken && updaterToken.length < 32) {
    throw new Error('MIZUKI_UPDATER_TOKEN must contain at least 32 characters');
  }
  const jobAuthoritySeed = env.MIZUKI_JOB_AUTHORITY_SEED;
  if (jobAuthoritySeed) {
    const decoded = Buffer.from(jobAuthoritySeed, 'base64');
    if (decoded.length !== 32 || decoded.toString('base64') !== jobAuthoritySeed) {
      throw new Error('MIZUKI_JOB_AUTHORITY_SEED must be canonical base64 for a 32-byte seed');
    }
  }
  const sseMaxConnections = boundedInteger(env.MIZUKI_SSE_MAX_CONNECTIONS, 100, 1, 1_000);
  const sseMaxConnectionsPerSource = boundedInteger(
    env.MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE,
    3,
    1,
    32,
  );
  if (sseMaxConnectionsPerSource > sseMaxConnections) {
    throw new Error('MIZUKI_SSE_MAX_CONNECTIONS_PER_SOURCE must not exceed the global limit');
  }
  const readinessRefreshMs = duration(env.MIZUKI_READINESS_REFRESH_MS, 30_000);
  const readinessMaxAgeMs = duration(env.MIZUKI_READINESS_MAX_AGE_MS, 90_000);
  const readinessTimeoutMs = duration(env.MIZUKI_READINESS_TIMEOUT_MS, 20_000);
  const escrowReadinessMinLamports = atomic(
    env.MIZUKI_ESCROW_READINESS_MIN_LAMPORTS,
    '1000000000',
    'MIZUKI_ESCROW_READINESS_MIN_LAMPORTS',
  );
  if (readinessMaxAgeMs < readinessRefreshMs) {
    throw new Error('MIZUKI_READINESS_MAX_AGE_MS must not be shorter than the refresh interval');
  }
  if (readinessTimeoutMs > readinessMaxAgeMs) {
    throw new Error('MIZUKI_READINESS_TIMEOUT_MS must not exceed the maximum evidence age');
  }

  return {
    host: env.MIZUKI_HOST ?? '127.0.0.1',
    port: int(env.MIZUKI_PORT, 8787),
    trustedProxyHops: boundedInteger(env.MIZUKI_TRUSTED_PROXY_HOPS, 0, 0, 1),
    trustedProxyConfigured: env.MIZUKI_TRUSTED_PROXY_HOPS !== undefined,
    webProxySecret: env.MIZUKI_WEB_PROXY_SECRET,
    rateLimitMaxSources: boundedInteger(env.MIZUKI_RATE_LIMIT_MAX_SOURCES, 10_000, 100, 100_000),
    sseMaxConnections,
    sseMaxConnectionsPerSource,
    sseIdleTimeoutMs: boundedInteger(env.MIZUKI_SSE_IDLE_TIMEOUT_MS, 120_000, 10_000, 3_600_000),
    readinessRefreshMs,
    readinessMaxAgeMs,
    readinessTimeoutMs,
    escrowReadinessMinLamports,
    publicBaseUrl: env.MIZUKI_PUBLIC_BASE_URL ?? 'http://127.0.0.1:8787',
    webOrigin: env.MIZUKI_WEB_ORIGIN,
    databaseUrl: env.MIZUKI_DATABASE_URL,
    adminToken: env.MIZUKI_ADMIN_TOKEN,
    codingGatewayUrl: httpUrl(env.MIZUKI_CODING_GATEWAY_URL ?? 'http://127.0.0.1:8642'),
    codingGatewayToken: env.MIZUKI_CODING_GATEWAY_TOKEN,
    updaterUrl,
    updaterToken,
    updaterTimeoutMs: duration(env.MIZUKI_UPDATER_TIMEOUT_MS, 8_000),
    updaterPollIntervalMs: duration(env.MIZUKI_UPDATER_POLL_INTERVAL_MS, 60_000),
    paymentMode: env.MIZUKI_PAYMENT_MODE === 'mock' ? ('mock' as const) : ('live' as const),
    payTo: env.MIZUKI_PAY_TO ?? '',
    facilitator: env.MIZUKI_X402_FACILITATOR ?? 'https://facilitator.payai.network',
    policySignerUrl: optionalHttpUrl(env.MIZUKI_POLICY_SIGNER_URL),
    policySignerToken: env.MIZUKI_POLICY_SIGNER_TOKEN,
    jobAuthoritySeed,
    refundUrl: env.MIZUKI_REFUND_URL ?? appendPath(env.MIZUKI_POLICY_SIGNER_URL, '/v1/refunds'),
    refundToken: env.MIZUKI_REFUND_TOKEN ?? env.MIZUKI_POLICY_SIGNER_TOKEN,
    escrowUrl:
      env.MIZUKI_ESCROW_SIGNER_URL ?? appendPath(env.MIZUKI_POLICY_SIGNER_URL, '/v1/escrows'),
    githubAppId: env.MIZUKI_GITHUB_APP_ID,
    githubPrivateKey: env.MIZUKI_GITHUB_PRIVATE_KEY?.replace(/\\n/g, '\n'),
    githubClientId: env.MIZUKI_GITHUB_CLIENT_ID,
    githubClientSecret: env.MIZUKI_GITHUB_CLIENT_SECRET,
    githubWebhookSecret: env.MIZUKI_GITHUB_WEBHOOK_SECRET,
    githubAuthorizationLabel: env.MIZUKI_GITHUB_AUTHORIZATION_LABEL ?? 'mizuki:authorized',
    sessionSecret: env.MIZUKI_SESSION_SECRET,
    requireGithubApp: env.MIZUKI_REQUIRE_GITHUB_APP !== '0',
    usePodBaseUrl: env.USEPOD_BASE_URL ?? 'https://api.usepod.ai/v1',
    usePodApiKey: env.USEPOD_API_KEY ?? '',
    usePodImplementationModel: env.USEPOD_MODEL ?? 'deepseek-v3.2',
    usePodModel:
      env.USEPOD_REVIEW_MODEL ??
      (env.MIZUKI_PAYMENT_MODE === 'mock' ? (env.USEPOD_MODEL ?? 'deepseek-v3.2') : ''),
    usePodInputUsdPerMillion: number(env.USEPOD_INPUT_USD_PER_MILLION, 0.2),
    usePodOutputUsdPerMillion: number(env.USEPOD_OUTPUT_USD_PER_MILLION, 0.4),
    internalRepos: new Set(
      (env.MIZUKI_INTERNAL_REPOS ?? '')
        .split(',')
        .map((value) => value.trim().toLowerCase())
        .filter(Boolean),
    ),
    tokenMint: env.MIZUKI_TOKEN_MINT,
    clawPumpBaseUrl: env.CLAWPUMP_BASE_URL ?? 'https://clawpump.tech',
    clawPumpApiKey: env.CLAWPUMP_API_KEY,
    clawPumpAgentId: env.CLAWPUMP_AGENT_ID,
    clawPumpPayoutWallet: env.CLAWPUMP_PAYOUT_WALLET,
  };
}

export function liveConfigIssues(config: Config): string[] {
  if (config.paymentMode !== 'live') return [];

  const missing: string[] = [];
  requireValue(missing, 'MIZUKI_DATABASE_URL', config.databaseUrl);
  requireHttps(missing, 'MIZUKI_PUBLIC_BASE_URL', config.publicBaseUrl);
  requireHttps(missing, 'MIZUKI_WEB_ORIGIN', config.webOrigin);
  requireHttps(missing, 'MIZUKI_X402_FACILITATOR', config.facilitator);
  requireHttps(missing, 'USEPOD_BASE_URL', config.usePodBaseUrl);
  requirePrivateService(missing, 'MIZUKI_CODING_GATEWAY_URL', config.codingGatewayUrl);
  requirePrivateService(missing, 'MIZUKI_POLICY_SIGNER_URL', config.policySignerUrl);
  requireSecret(missing, 'MIZUKI_POLICY_SIGNER_TOKEN', config.policySignerToken);
  requireValue(missing, 'MIZUKI_JOB_AUTHORITY_SEED', config.jobAuthoritySeed);
  requireSecret(missing, 'MIZUKI_ADMIN_TOKEN', config.adminToken);
  requireSecret(missing, 'MIZUKI_CODING_GATEWAY_TOKEN', config.codingGatewayToken);
  requireValue(missing, 'USEPOD_API_KEY', config.usePodApiKey);
  requireValue(missing, 'USEPOD_MODEL', config.usePodImplementationModel);
  requireValue(missing, 'USEPOD_REVIEW_MODEL', config.usePodModel);
  requireValue(missing, 'MIZUKI_GITHUB_APP_ID', config.githubAppId);
  requireValue(missing, 'MIZUKI_GITHUB_PRIVATE_KEY', config.githubPrivateKey);
  requireValue(missing, 'MIZUKI_GITHUB_CLIENT_ID', config.githubClientId);
  requireSecret(missing, 'MIZUKI_GITHUB_CLIENT_SECRET', config.githubClientSecret);
  requireSecret(missing, 'MIZUKI_GITHUB_WEBHOOK_SECRET', config.githubWebhookSecret);
  requireSecret(missing, 'MIZUKI_SESSION_SECRET', config.sessionSecret);
  requireSecret(missing, 'MIZUKI_WEB_PROXY_SECRET', config.webProxySecret);
  requirePrivateService(missing, 'MIZUKI_UPDATER_URL', config.updaterUrl);
  requireSecret(missing, 'MIZUKI_UPDATER_TOKEN', config.updaterToken);

  if (!config.trustedProxyConfigured || config.trustedProxyHops !== 1) {
    missing.push('MIZUKI_TRUSTED_PROXY_HOPS=1');
  }
  if (config.clawPumpAgentId) {
    try {
      address(config.clawPumpPayoutWallet ?? '');
    } catch {
      missing.push('CLAWPUMP_PAYOUT_WALLET');
    }
  }

  if (!config.requireGithubApp) missing.push('MIZUKI_REQUIRE_GITHUB_APP=1');
  if (config.usePodImplementationModel === config.usePodModel) {
    missing.push('USEPOD_REVIEW_MODEL must differ from USEPOD_MODEL');
  }
  try {
    address(config.payTo);
  } catch {
    missing.push('MIZUKI_PAY_TO');
  }
  if (!config.githubPrivateKey?.includes('BEGIN') || !config.githubPrivateKey.includes('KEY')) {
    missing.push('MIZUKI_GITHUB_PRIVATE_KEY must be PEM');
  }
  return [...new Set(missing)];
}

export function assertLiveConfig(config: Config): void {
  const issues = liveConfigIssues(config);
  if (issues.length === 0) return;
  throw new Error(`live Mizuki configuration is incomplete: ${issues.join(', ')}`);
}

export function assertBootConfig(config: Config): void {
  if (config.paymentMode !== 'live') return;
  const missing: string[] = [];
  requireValue(missing, 'MIZUKI_DATABASE_URL', config.databaseUrl);
  requireSecret(missing, 'MIZUKI_ADMIN_TOKEN', config.adminToken);
  if (missing.length > 0) {
    throw new Error(`live Mizuki boot configuration is incomplete: ${missing.join(', ')}`);
  }
}

function appendPath(base: string | undefined, path: string): string | undefined {
  return base ? `${httpUrl(base).replace(/\/$/, '')}${path}` : undefined;
}

function optionalHttpUrl(value: string | undefined): string | undefined {
  return value ? httpUrl(value) : undefined;
}

function httpUrl(value: string): string {
  const normalized = /^[a-z][a-z0-9+.-]*:\/\//i.test(value) ? value : `http://${value}`;
  const url = new URL(normalized);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
    throw new Error('service URL must use HTTP without embedded credentials');
  }
  return normalized.replace(/\/$/, '');
}

function int(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65_535) {
    throw new Error(`invalid positive integer: ${value}`);
  }
  return parsed;
}

function boundedInteger(
  value: string | undefined,
  fallback: number,
  min: number,
  max: number,
): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`invalid integer between ${min} and ${max}: ${value}`);
  }
  return parsed;
}

function number(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0)
    throw new Error(`invalid non-negative number: ${value}`);
  return parsed;
}

function atomic(value: string | undefined, fallback: string, name: string): string {
  const parsed = value ?? fallback;
  if (!/^[1-9][0-9]*$/.test(parsed)) throw new Error(`${name} must be a positive atomic amount`);
  return parsed;
}

function duration(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed < 250 || parsed > 24 * 60 * 60_000) {
    throw new Error(`invalid duration in milliseconds: ${value}`);
  }
  return parsed;
}

function requireValue(missing: string[], name: string, value: string | undefined): void {
  if (!value?.trim()) missing.push(name);
}

function requireSecret(missing: string[], name: string, value: string | undefined): void {
  if (!value || value.length < 32) missing.push(name);
}

function requireHttps(missing: string[], name: string, value: string | undefined): void {
  try {
    if (!value || new URL(value).protocol !== 'https:') missing.push(name);
  } catch {
    missing.push(name);
  }
}

function requirePrivateService(missing: string[], name: string, value: string | undefined): void {
  try {
    if (!value) throw new Error('missing service URL');
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    if (
      url.protocol !== 'http:' ||
      !url.port ||
      host.includes('.') ||
      host === 'localhost' ||
      url.pathname !== '/' ||
      url.search ||
      url.hash
    ) {
      missing.push(name);
    }
  } catch {
    missing.push(name);
  }
}
