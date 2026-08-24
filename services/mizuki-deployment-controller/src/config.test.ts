import { describe, expect, it } from 'vitest';
import { loadConfig } from './config.js';

const databaseUrl = ['postgres://controller', ':secret', '@db.internal/controller'].join('');
const base = {
  NODE_ENV: 'production',
  MIZUKI_DEPLOY_AUTH_TOKEN: 'a'.repeat(32),
  MIZUKI_DEPLOY_DATABASE_URL: databaseUrl,
  MIZUKI_DEPLOY_DATABASE_SSL_MODE: 'verify-full',
  MIZUKI_DEPLOY_REPOSITORY: 'open-covenant/covenant',
  MIZUKI_DEPLOY_IMAGE_REPOSITORY: 'ghcr.io/open-covenant/mizuki-api',
  MIZUKI_DEPLOY_RENDER_API_KEY: 'r'.repeat(32),
  MIZUKI_DEPLOY_RENDER_SHADOW_SERVICE_ID: 'srv-shadow123',
  MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID: 'srv-production123',
  MIZUKI_DEPLOY_RENDER_ALLOWED_SERVICE_IDS: 'srv-shadow123,srv-production123',
  MIZUKI_DEPLOY_ARTIFACT_ORIGINS: 'https://github.com,https://objects.githubusercontent.com',
  MIZUKI_DEPLOY_SHADOW_PROBE_URL: 'http://mizuki-shadow:10000/deployz',
  MIZUKI_DEPLOY_PRODUCTION_PROBE_URL: 'https://mizuki.example/internal/mizuki/functional-readiness',
  MIZUKI_DEPLOY_PRODUCTION_PROBE_TOKEN: 'p'.repeat(32),
};

describe('deployment controller config', () => {
  it('loads an exact image-backed two-service production boundary', () => {
    expect(loadConfig(base)).toMatchObject({
      repository: 'open-covenant/covenant',
      imageRepository: 'ghcr.io/open-covenant/mizuki-api',
      renderApiUrl: 'https://api.render.com/v1',
      shadowServiceId: 'srv-shadow123',
      productionServiceId: 'srv-production123',
      database: { sslMode: 'verify-full', connectionTimeoutMs: 10_000 },
      minPromotionAgeMs: 10_000,
    });
  });

  it('rejects service aliases and extra allowlist entries', () => {
    expect(() =>
      loadConfig({ ...base, MIZUKI_DEPLOY_RENDER_PRODUCTION_SERVICE_ID: 'srv-shadow123' }),
    ).toThrow('must be distinct');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_DEPLOY_RENDER_ALLOWED_SERVICE_IDS: 'srv-shadow123,srv-production123,srv-other123',
      }),
    ).toThrow('exactly');
  });

  it('rejects alternate production API, artifact destinations, and mutable image tags', () => {
    expect(() =>
      loadConfig({ ...base, MIZUKI_DEPLOY_RENDER_API_URL: 'https://render-proxy.example/v1' }),
    ).toThrow('official Render API');
    expect(() =>
      loadConfig({ ...base, MIZUKI_DEPLOY_ARTIFACT_ORIGINS: 'http://artifacts.example' }),
    ).toThrow('HTTPS origins');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_DEPLOY_IMAGE_REPOSITORY: 'ghcr.io/open-covenant/mizuki-api:latest',
      }),
    ).toThrow('invalid');
  });

  it('requires explicit TLS policy and role-specific probe paths', () => {
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_DEPLOY_DATABASE_URL: `${databaseUrl}?sslmode=disable`,
      }),
    ).toThrow('explicit controller configuration');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_DEPLOY_PRODUCTION_PROBE_URL: 'https://mizuki.example/healthz',
      }),
    ).toThrow('/internal/mizuki/functional-readiness');
    expect(() =>
      loadConfig({
        ...base,
        MIZUKI_DEPLOY_SHADOW_PROBE_URL:
          'http://mizuki-shadow:10000/internal/mizuki/functional-readiness',
      }),
    ).toThrow('/deployz');
    expect(() => loadConfig({ ...base, MIZUKI_DEPLOY_PROBE_TOKEN: 'p'.repeat(32) })).toThrow(
      'production-only token',
    );
  });
});
