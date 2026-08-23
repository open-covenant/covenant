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
  MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: '127.0.0.1:9000',
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
    expect(config.operational).toMatchObject({
      shadowHookUrl: 'http://127.0.0.1:9000/v1/deployments/shadow',
      shadowHealthUrlTemplate: 'http://127.0.0.1:9000/v1/deployments/shadow/{deploymentId}/health',
      promoteHookUrl: 'http://127.0.0.1:9000/v1/deployments/promote',
      promotionHealthUrlTemplate:
        'http://127.0.0.1:9000/v1/deployments/production/{deploymentId}/health',
      rollbackHookUrl: 'http://127.0.0.1:9000/v1/deployments/rollback',
      deployReadinessUrl: 'http://127.0.0.1:9000/readyz',
    });
  });

  it('boots closed and reports every missing operational input', () => {
    const config = loadConfig({
      ...BASE_ENV,
      MIZUKI_UPDATER_PROPOSAL_KEYS_JSON: '',
      MIZUKI_UPDATER_GITHUB_PRIVATE_KEY: undefined,
      MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: '   ',
      MIZUKI_UPDATER_DEPLOY_HOOK_TOKEN: undefined,
    });

    expect(config.operational).toBeUndefined();
    expect(config.operationalFailures).toEqual([
      'MIZUKI_UPDATER_PROPOSAL_KEYS_JSON',
      'MIZUKI_UPDATER_GITHUB_PRIVATE_KEY',
      'MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT',
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

  it('requires an origin-only artifact allowlist', () => {
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

  it('does not let configuration choose controller paths or credentials', () => {
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: 'http://user:secret@127.0.0.1:9000',
      }),
    ).toThrow('credential-free host and port');
    expect(() =>
      loadConfig({
        ...BASE_ENV,
        MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: 'http://127.0.0.1:9000/custom/path',
      }),
    ).toThrow('credential-free host and port');
  });

  it('pins production to the Render private service origin', () => {
    const production = {
      ...BASE_ENV,
      NODE_ENV: 'production',
      MIZUKI_UPDATER_MEMORY_STORE: 'false',
      MIZUKI_UPDATER_DATABASE_URL: 'postgres://mizuki:secret@postgres:5432/updater',
      MIZUKI_UPDATER_HOST: '0.0.0.0',
      MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: 'mizuki-deployment-controller:8794',
    };
    expect(loadConfig(production).operational?.deployReadinessUrl).toBe(
      'http://mizuki-deployment-controller:8794/readyz',
    );
    expect(() =>
      loadConfig({
        ...production,
        MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: 'https://deploy.example.test',
      }),
    ).toThrow('fixed Render private origin');
    expect(() =>
      loadConfig({
        ...production,
        MIZUKI_UPDATER_DEPLOY_CONTROLLER_HOSTPORT: 'mizuki-deployment-controller:8795',
      }),
    ).toThrow('fixed Render private origin');
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
