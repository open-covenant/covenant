import { describe, expect, it } from 'vitest';
import { loadConfig } from './config.js';

const BASE_ENV: NodeJS.ProcessEnv = {
  NODE_ENV: 'test',
  MIZUKI_UPDATER_AUTH_TOKEN: 'a'.repeat(32),
  MIZUKI_UPDATER_READ_TOKEN: 'r'.repeat(32),
  MIZUKI_UPDATER_MEMORY_STORE: 'true',
  MIZUKI_UPDATER_PROPOSAL_KEYS_JSON: JSON.stringify({ key: 'p'.repeat(64) }),
  MIZUKI_UPDATER_BENCHMARK_KEYS_JSON: JSON.stringify({ key: 'b'.repeat(64) }),
  MIZUKI_UPDATER_REVIEW_KEYS_JSON: JSON.stringify({ key: 'r'.repeat(64) }),
  MIZUKI_UPDATER_ALLOWED_REPOSITORIES: 'mizuki-labs/mizuki',
  MIZUKI_UPDATER_ALLOWED_BASE_BRANCHES: 'main',
  MIZUKI_UPDATER_HEAD_BRANCH_PREFIX: 'mizuki/',
  MIZUKI_UPDATER_MANDATORY_CHECKS: 'test,security',
  MIZUKI_UPDATER_ARTIFACT_ORIGINS: 'https://artifacts.example.test',
  MIZUKI_UPDATER_GITHUB_APP_ID: '123',
  MIZUKI_UPDATER_GITHUB_PRIVATE_KEY: 'r'.repeat(64),
  MIZUKI_UPDATER_SHADOW_HOOK_URL: 'http://127.0.0.1:9000/shadow',
  MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE:
    'http://127.0.0.1:9000/deployments/{deploymentId}/health',
  MIZUKI_UPDATER_PROMOTE_HOOK_URL: 'http://127.0.0.1:9000/promote',
  MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE:
    'http://127.0.0.1:9000/production/{deploymentId}/health',
  MIZUKI_UPDATER_ROLLBACK_HOOK_URL: 'http://127.0.0.1:9000/rollback',
  MIZUKI_UPDATER_DEPLOY_READINESS_URL: 'http://127.0.0.1:9000/readyz',
  MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: 'd'.repeat(32),
};

describe('updater configuration', () => {
  it('allows an in-memory loopback service outside production', () => {
    const config = loadConfig(BASE_ENV);
    expect(config.memoryStore).toBe(true);
    expect(config.allowedRepositories).toEqual(new Set(['mizuki-labs/mizuki']));
    expect(config.artifactOrigins).toEqual(new Set(['https://artifacts.example.test']));
    expect(config.operational).toBeDefined();
    expect(config.operationalFailures).toEqual([]);
  });

  it('boots closed and reports every missing operational input', () => {
    const config = loadConfig({
      ...BASE_ENV,
      MIZUKI_UPDATER_PROPOSAL_KEYS_JSON: '',
      MIZUKI_UPDATER_GITHUB_PRIVATE_KEY: undefined,
      MIZUKI_UPDATER_DEPLOY_READINESS_URL: '   ',
      MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: undefined,
    });

    expect(config.operational).toBeUndefined();
    expect(config.operationalFailures).toEqual([
      'MIZUKI_UPDATER_PROPOSAL_KEYS_JSON',
      'MIZUKI_UPDATER_GITHUB_PRIVATE_KEY',
      'MIZUKI_UPDATER_DEPLOY_READINESS_URL',
      'MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN',
    ]);
  });

  it('requires Postgres unless the local memory store is explicitly enabled', () => {
    expect(() => loadConfig({ ...BASE_ENV, MIZUKI_UPDATER_MEMORY_STORE: 'false' })).toThrow(
      'MIZUKI_UPDATER_DATABASE_URL is required',
    );
  });

  it('disables the memory store in production and on non-loopback hosts', () => {
    expect(() => loadConfig({ ...BASE_ENV, NODE_ENV: 'production' })).toThrow(
      'Memory store is disabled in production',
    );
    expect(() => loadConfig({ ...BASE_ENV, MIZUKI_UPDATER_HOST: '0.0.0.0' })).toThrow(
      'Memory store must bind to a loopback address',
    );
  });

  it('requires an exact health template and origin-only artifact allowlist', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE: 'http://127.0.0.1:9000/health',
      }),
    ).toThrow('Shadow health URL template must contain {deploymentId}');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_ARTIFACT_ORIGINS: 'https://artifacts.example.test/path',
      }),
    ).toThrow('Artifact origins must not contain credentials, a path');
  });

  it('requires separate submission, read, and deployment credentials', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: BASE_ENV.MIZUKI_UPDATER_AUTH_TOKEN,
      }),
    ).toThrow('Submission, read, and deployment tokens must be distinct');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_READ_TOKEN: BASE_ENV.MIZUKI_UPDATER_AUTH_TOKEN,
      }),
    ).toThrow('Submission, read, and deployment tokens must be distinct');
  });

  it('does not let deployment receipts choose the health credential origin', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE: 'http://{deploymentId}.example.test/health',
      }),
    ).toThrow('deployment ID must appear only in the URL path');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_PROMOTE_HOOK_URL: 'http://127.0.0.2:9000/promote',
      }),
    ).toThrow('Deployment endpoints must use one origin');
  });

  it('requires distinct fixed-origin shadow and production health evidence', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE:
          BASE_ENV.MIZUKI_UPDATER_SHADOW_HEALTH_URL_TEMPLATE,
      }),
    ).toThrow('Shadow and promotion health URL paths must differ');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE:
          'http://127.0.0.1:9000/deployments/{deploymentId}/health?environment=production',
      }),
    ).toThrow('Shadow and promotion health URL paths must differ');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_PROMOTION_HEALTH_URL_TEMPLATE: 'http://{deploymentId}.example.test/health',
      }),
    ).toThrow('Promotion health deployment ID must appear only in the URL path');
  });

  it('requires the promotion deadline to permit a final soak poll', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_POLL_INTERVAL_MS: '5000',
        MIZUKI_UPDATER_PROMOTION_SOAK_MS: '60000',
        MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS: '64999',
      }),
    ).toThrow('Promotion timeout must exceed the soak by at least one poll interval');

    expect(
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_POLL_INTERVAL_MS: '5000',
        MIZUKI_UPDATER_PROMOTION_SOAK_MS: '60000',
        MIZUKI_UPDATER_PROMOTION_TIMEOUT_MS: '65000',
      }),
    ).toMatchObject({ promotionSoakMs: 60_000, promotionTimeoutMs: 65_000 });
  });
});
