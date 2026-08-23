import { randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { describe, expect, it } from 'vitest';
import type { DatabaseConfig } from './config.js';
import { operationEvent, type DeploymentOperation } from './domain.js';
import { PostgresOperationStore } from './store.js';

const testDatabaseUrl = process.env.MIZUKI_DEPLOY_TEST_DATABASE_URL;
const integrationTest = testDatabaseUrl ? it : it.skip;

describe('Postgres operation ledger integration', () => {
  integrationTest(
    'migrates and preserves immutable state and events across restart',
    async () => {
      const schema = `mizuki_deploy_test_${randomUUID().replaceAll('-', '')}`;
      const sslMode = testSslMode();
      const admin = new Pool({
        connectionString: testDatabaseUrl!,
        connectionTimeoutMillis: 10_000,
        ssl: sslMode === 'disable' ? false : { rejectUnauthorized: sslMode === 'verify-full' },
      });
      let store: PostgresOperationStore | null = null;
      try {
        await admin.query(`CREATE SCHEMA ${quoteIdentifier(schema)}`);
        const config = databaseConfig(testDatabaseUrl!, schema, sslMode);
        store = new PostgresOperationStore(config);
        await store.migrate();

        const operation = fixtureOperation();
        await store.insert(operation, operationEvent(operation, 'shadow_reserved'));
        operation.artifactVerifiedAt = new Date('2026-08-23T12:01:00.000Z');
        operation.shadowState = 'triggering';
        operation.shadowStartedAt = new Date('2026-08-23T12:02:00.000Z');
        operation.updatedAt = operation.shadowStartedAt;
        await store.save(operation, operationEvent(operation, 'shadow_triggering'));
        await store.close();
        store = null;

        store = new PostgresOperationStore(config);
        await store.migrate();
        await expect(store.get(operation.upgradeId)).resolves.toMatchObject({
          upgradeId: operation.upgradeId,
          shadowState: 'triggering',
          shadowStartedAt: operation.shadowStartedAt,
        });
        await expect(store.events(operation.upgradeId)).resolves.toMatchObject([
          { type: 'shadow_reserved' },
          { type: 'shadow_triggering' },
        ]);

        await expect(
          admin.query(
            `UPDATE ${quoteIdentifier(schema)}.mizuki_deployment_events SET event_type = 'tampered'`,
          ),
        ).rejects.toThrow('deployment events are append-only');
        await expect(
          admin.query(`DELETE FROM ${quoteIdentifier(schema)}.mizuki_deployment_events`),
        ).rejects.toThrow('deployment events are append-only');
      } finally {
        await store?.close();
        await admin.query(`DROP SCHEMA IF EXISTS ${quoteIdentifier(schema)} CASCADE`);
        await admin.end();
      }
    },
    30_000,
  );
});

function databaseConfig(
  connectionString: string,
  schema: string,
  sslMode: DatabaseConfig['sslMode'],
): DatabaseConfig {
  const url = new URL(connectionString);
  url.searchParams.set('options', `-csearch_path=${schema}`);
  return {
    connectionString: url.href,
    sslMode,
    connectionTimeoutMs: 10_000,
    maxConnections: 4,
  };
}

function testSslMode(): DatabaseConfig['sslMode'] {
  const value = process.env.MIZUKI_DEPLOY_TEST_DATABASE_SSL_MODE ?? 'disable';
  if (value === 'disable' || value === 'require' || value === 'verify-full') return value;
  throw new Error('MIZUKI_DEPLOY_TEST_DATABASE_SSL_MODE is invalid');
}

function quoteIdentifier(value: string): string {
  if (!/^[a-z][a-z0-9_]+$/.test(value)) throw new Error('Invalid test schema');
  return `"${value}"`;
}

function fixtureOperation(): DeploymentOperation {
  const now = new Date('2026-08-23T12:00:00.000Z');
  return {
    upgradeId: 'integration-upgrade',
    proposalId: 'integration-proposal',
    repository: 'open-covenant/covenant',
    manifestSha256: 'e'.repeat(64),
    candidateSha: 'a'.repeat(40),
    artifactUrl: 'https://objects.githubusercontent.com/manifest.json',
    artifactSha256: 'f'.repeat(64),
    artifactSizeBytes: 100,
    imageRef: `ghcr.io/open-covenant/mizuki-api@sha256:${'f'.repeat(64)}`,
    artifactVerifiedAt: null,
    prNumber: 42,
    shadowIdempotencyKey: 'integration-upgrade:shadow',
    shadowRequestHash: 'd'.repeat(64),
    shadowState: 'reserved',
    shadowServiceFingerprint: '1'.repeat(64),
    shadowBaselineDeployId: 'dep-shadow-baseline',
    shadowBaselineArtifactSha256: 'c'.repeat(64),
    shadowStartedAt: null,
    shadowDeployId: null,
    shadowActive: true,
    shadowRestoreState: null,
    shadowRestoreStartedAt: null,
    shadowRestoreDeployId: null,
    promotionIdempotencyKey: null,
    promotionRequestHash: null,
    promotionState: null,
    mergeSha: null,
    productionServiceFingerprint: null,
    productionBaselineDeployId: null,
    productionBaselineArtifactSha256: null,
    promotionStartedAt: null,
    promotionDeployId: null,
    productionActive: false,
    rollbackIdempotencyKey: null,
    rollbackRequestHash: null,
    rollbackState: null,
    rollbackStartedAt: null,
    rollbackDeployId: null,
    createdAt: now,
    updatedAt: now,
  };
}
