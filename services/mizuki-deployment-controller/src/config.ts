import { z } from 'zod';

const renderServiceId = z.string().regex(/^srv-[a-z0-9]+$/);
const repository = z
  .string()
  .toLowerCase()
  .regex(/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/);
const sslMode = z.enum(['disable', 'require', 'verify-full']);

const schema = z
  .object({
    NODE_ENV: z.enum(['development', 'test', 'production']).default('development'),
    MIZUKI_DEPLOY_HOST: z.string().default('127.0.0.1'),
    MIZUKI_DEPLOY_PORT: z.coerce.number().int().min(1).max(65_535).default(8794),
    MIZUKI_DEPLOY_AUTH_TOKEN: z.string().min(32),
    MIZUKI_DEPLOY_DATABASE_URL: z.string().url(),
    MIZUKI_DEPLOY_DATABASE_SSL_MODE: sslMode.default('verify-full'),
    MIZUKI_DEPLOY_DATABASE_CONNECT_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(60_000)
      .default(10_000),
    MIZUKI_DEPLOY_DATABASE_MAX_CONNECTIONS: z.coerce.number().int().min(2).max(20).default(8),
    MIZUKI_DEPLOY_REPOSITORY: repository,
    MIZUKI_DEPLOY_IMAGE_REPOSITORY: z.string().min(3).max(255),
    MIZUKI_DEPLOY_RENDER_API_KEY: z.string().min(20),
    MIZUKI_DEPLOY_RENDER_API_URL: z.string().url().default('https://api.render.com/v1'),
    MIZUKI_DEPLOY_RENDER_SHADOW_SERVICE_ID: renderServiceId,
    MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID: renderServiceId,
    MIZUKI_DEPLOY_RENDER_ALLOWED_SERVICE_IDS: z.string().min(1),
    MIZUKI_DEPLOY_ARTIFACT_ORIGINS: z.string().min(8),
    MIZUKI_DEPLOY_SHADOW_PROBE_URL: z.string().url(),
    MIZUKI_DEPLOY_PRODUCTION_PROBE_URL: z.string().url(),
    MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN: z.string().min(32),
    MIZUKI_DEPLOY_RENDER_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(120_000)
      .default(20_000),
    MIZUKI_DEPLOY_ARTIFACT_TIMEOUT_MS: z.coerce
      .number()
      .int()
      .min(1_000)
      .max(120_000)
      .default(30_000),
    MIZUKI_DEPLOY_PROBE_TIMEOUT_MS: z.coerce.number().int().min(1_000).max(30_000).default(10_000),
    MIZUKI_DEPLOY_RECONCILIATION_GRACE_MS: z.coerce
      .number()
      .int()
      .min(10_000)
      .max(10 * 60_000)
      .default(120_000),
    MIZUKI_DEPLOY_MIN_PROMOTION_AGE_MS: z.coerce
      .number()
      .int()
      .min(10_000)
      .max(30 * 60_000)
      .default(120_000),
  })
  .passthrough();

export interface DatabaseConfig {
  connectionString: string;
  sslMode: z.infer<typeof sslMode>;
  connectionTimeoutMs: number;
  maxConnections: number;
}

export interface ControllerConfig {
  environment: 'development' | 'test' | 'production';
  host: string;
  port: number;
  authToken: string;
  database: DatabaseConfig;
  repository: string;
  imageRepository: string;
  renderApiKey: string;
  renderApiUrl: string;
  shadowServiceId: string;
  productionServiceId: string;
  allowedServiceIds: Set<string>;
  artifactOrigins: Set<string>;
  shadowProbeUrl: string;
  productionProbeUrl: string;
  productionProbeToken: string;
  renderTimeoutMs: number;
  artifactTimeoutMs: number;
  probeTimeoutMs: number;
  reconciliationGraceMs: number;
  minPromotionAgeMs: number;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): ControllerConfig {
  if (env.MIZUKI_DEPLOY_PROBE_TOKEN !== undefined) {
    throw new Error('MIZUKI_DEPLOY_PROBE_TOKEN is not allowed; use the production-only token');
  }
  const parsed = schema.parse(env);
  const allowedServiceIds = csvSet(parsed.MIZUKI_DEPLOY_RENDER_ALLOWED_SERVICE_IDS);
  for (const id of allowedServiceIds) renderServiceId.parse(id);
  const required = new Set([
    parsed.MIZUKI_DEPLOY_RENDER_SHADOW_SERVICE_ID,
    parsed.MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID,
  ]);
  if (required.size !== 2) throw new Error('Shadow and production services must be distinct');
  if (
    allowedServiceIds.size !== required.size ||
    [...required].some((id) => !allowedServiceIds.has(id))
  ) {
    throw new Error('Render service allowlist must contain exactly the shadow and production IDs');
  }

  const artifactOrigins = new Set(
    [...csvSet(parsed.MIZUKI_DEPLOY_ARTIFACT_ORIGINS)].map((value) => {
      const url = new URL(value);
      if (
        url.protocol !== 'https:' ||
        url.username ||
        url.password ||
        url.pathname !== '/' ||
        url.search ||
        url.hash
      ) {
        throw new Error('Artifact origins must be credential-free HTTPS origins');
      }
      return url.origin;
    }),
  );
  const api = new URL(parsed.MIZUKI_DEPLOY_RENDER_API_URL);
  if (api.username || api.password || api.search || api.hash) {
    throw new Error('Render API URL must not include credentials, query, or fragment');
  }
  const normalizedApi = api.href.replace(/\/$/, '');
  if (parsed.NODE_ENV === 'production' && normalizedApi !== 'https://api.render.com/v1') {
    throw new Error('Production must use the official Render API origin');
  }

  const databaseUrl = new URL(parsed.MIZUKI_DEPLOY_DATABASE_URL);
  if (!['postgres:', 'postgresql:'].includes(databaseUrl.protocol)) {
    throw new Error('Deployment database must use PostgreSQL');
  }
  for (const key of ['sslmode', 'sslcert', 'sslkey', 'sslrootcert']) {
    if (databaseUrl.searchParams.has(key)) {
      throw new Error('Database TLS settings must use explicit controller configuration');
    }
  }
  if (
    parsed.NODE_ENV === 'production' &&
    parsed.MIZUKI_DEPLOY_DATABASE_SSL_MODE === 'disable' &&
    !privateHost(databaseUrl.hostname)
  ) {
    throw new Error('Production database TLS may be disabled only on a private network host');
  }

  return {
    environment: parsed.NODE_ENV,
    host: parsed.MIZUKI_DEPLOY_HOST,
    port: parsed.MIZUKI_DEPLOY_PORT,
    authToken: parsed.MIZUKI_DEPLOY_AUTH_TOKEN,
    database: {
      connectionString: parsed.MIZUKI_DEPLOY_DATABASE_URL,
      sslMode: parsed.MIZUKI_DEPLOY_DATABASE_SSL_MODE,
      connectionTimeoutMs: parsed.MIZUKI_DEPLOY_DATABASE_CONNECT_TIMEOUT_MS,
      maxConnections: parsed.MIZUKI_DEPLOY_DATABASE_MAX_CONNECTIONS,
    },
    repository: parsed.MIZUKI_DEPLOY_REPOSITORY,
    imageRepository: normalizeImageRepository(parsed.MIZUKI_DEPLOY_IMAGE_REPOSITORY),
    renderApiKey: parsed.MIZUKI_DEPLOY_RENDER_API_KEY,
    renderApiUrl: normalizedApi,
    shadowServiceId: parsed.MIZUKI_DEPLOY_RENDER_SHADOW_SERVICE_ID,
    productionServiceId: parsed.MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID,
    allowedServiceIds,
    artifactOrigins,
    shadowProbeUrl: probeUrl(parsed.MIZUKI_DEPLOY_SHADOW_PROBE_URL, parsed.NODE_ENV, 'shadow'),
    productionProbeUrl: probeUrl(
      parsed.MIZUKI_DEPLOY_PRODUCTION_PROBE_URL,
      parsed.NODE_ENV,
      'production',
    ),
    productionProbeToken: parsed.MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN,
    renderTimeoutMs: parsed.MIZUKI_DEPLOY_RENDER_TIMEOUT_MS,
    artifactTimeoutMs: parsed.MIZUKI_DEPLOY_ARTIFACT_TIMEOUT_MS,
    probeTimeoutMs: parsed.MIZUKI_DEPLOY_PROBE_TIMEOUT_MS,
    reconciliationGraceMs: parsed.MIZUKI_DEPLOY_RECONCILIATION_GRACE_MS,
    minPromotionAgeMs: parsed.MIZUKI_DEPLOY_MIN_PROMOTION_AGE_MS,
  };
}

export function normalizeImageRepository(value: string): string {
  if (value !== value.toLowerCase() || value.includes('@') || value.includes('://')) {
    throw new Error('Image repository must be a lowercase registry path without a digest');
  }
  const slash = value.indexOf('/');
  if (slash < 1 || slash === value.length - 1) {
    throw new Error('Image repository must include a registry and repository path');
  }
  const host = value.slice(0, slash);
  const path = value.slice(slash + 1);
  if (
    !/^[a-z0-9.-]+(?::[0-9]{1,5})?$/.test(host) ||
    !/^[a-z0-9]+(?:[._-][a-z0-9]+)*(?:\/[a-z0-9]+(?:[._-][a-z0-9]+)*)+$/.test(path) ||
    /:[^/]+$/.test(path)
  ) {
    throw new Error('Image repository is invalid');
  }
  return `${host}/${path}`;
}

function probeUrl(
  value: string,
  environment: ControllerConfig['environment'],
  role: 'shadow' | 'production',
) {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash) {
    throw new Error('Application probe URLs must not include credentials, query, or fragment');
  }
  const expectedPath = role === 'shadow' ? '/deployz' : '/internal/mizuki/functional-readiness';
  if (url.pathname !== expectedPath) {
    throw new Error(`${role} application probe URL must use ${expectedPath}`);
  }
  const privateHttp = role === 'shadow' && url.protocol === 'http:' && privateHost(url.hostname);
  if (url.protocol !== 'https:' && !(environment !== 'production' || privateHttp)) {
    throw new Error('Production application probes must use HTTPS or private shadow HTTP');
  }
  return url.href;
}

function privateHost(hostname: string): boolean {
  return !hostname.includes('.') || hostname.endsWith('.internal');
}

function csvSet(value: string): Set<string> {
  const values = value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
  if (values.length === 0 || new Set(values).size !== values.length) {
    throw new Error('Configuration lists must be non-empty and unique');
  }
  return new Set(values);
}
