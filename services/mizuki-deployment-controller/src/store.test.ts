import type { QueryResult } from 'pg';
import { describe, expect, it } from 'vitest';
import type { DatabaseConfig } from './config.js';
import { operationEvent, type DeploymentOperation } from './domain.js';
import {
  PostgresOperationStore,
  postgresPoolConfig,
  type PostgresClient,
  type PostgresPool,
} from './store.js';

const database: DatabaseConfig = {
  connectionString: ['postgres://controller', ':secret', '@db.internal/controller'].join(''),
  sslMode: 'verify-full',
  connectionTimeoutMs: 7_500,
  maxConnections: 6,
};

describe('Postgres operation ledger', () => {
  it('configures bounded connections and explicit TLS verification', () => {
    expect(postgresPoolConfig(database)).toMatchObject({
      max: 6,
      connectionTimeoutMillis: 7_500,
      statement_timeout: 15_000,
      idle_in_transaction_session_timeout: 15_000,
      keepAlive: true,
      ssl: { rejectUnauthorized: true },
    });
    expect(postgresPoolConfig({ ...database, sslMode: 'disable' }).ssl).toBe(false);
  });

  it('installs the immutable event ledger as a checksummed migration', async () => {
    const pool = new FakePool();
    const store = new PostgresOperationStore(database, pool);
    await store.migrate();

    const sql = pool.clients[0].statements.map(({ text }) => text).join('\n');
    expect(sql).toContain('CREATE TABLE mizuki_deployment_events');
    expect(sql).toContain('BEFORE UPDATE OR DELETE ON mizuki_deployment_events');
    expect(
      pool.clients[0].statements.some(({ values }) =>
        values?.includes('append_only_operation_events'),
      ),
    ).toBe(true);
    expect(pool.clients[0].statements.at(-1)?.text).toBe('COMMIT');
  });

  it('writes operation state and its event atomically', async () => {
    const pool = new FakePool();
    const store = new PostgresOperationStore(database, pool);
    const operation = fixtureOperation();
    await store.insert(operation, operationEvent(operation, 'shadow_reserved'));

    expect(pool.clients[0].statements.map(({ text }) => compact(text))).toEqual([
      'BEGIN',
      expect.stringContaining('INSERT INTO mizuki_deployment_operations'),
      expect.stringContaining('INSERT INTO mizuki_deployment_events'),
      'COMMIT',
    ]);
  });

  it('keeps shadow adoption evidence out of the strict operation record', async () => {
    const pool = new FakePool();
    const store = new PostgresOperationStore(database, pool);
    const operation = fixtureOperation();
    operation.shadowState = 'completed';
    operation.shadowRestoreState = 'failed';
    operation.shadowRestoreDeployId = 'dep-restore-failed';
    operation.shadowActive = false;
    const event = operationEvent(operation, 'shadow_baseline_adopted', {
      idempotencyKey: 'upgrade-1:adopt-shadow',
      requestHash: '9'.repeat(64),
    });

    await store.save(operation, event);

    const statements = pool.clients[0].statements;
    const update = statements.find(({ text }) => text.includes('UPDATE mizuki_deployment_operations'));
    const append = statements.find(({ text }) => text.includes('INSERT INTO mizuki_deployment_events'));
    const record = JSON.parse(String(update?.values?.[9])) as Record<string, unknown>;
    const detail = JSON.parse(String(append?.values?.[3])) as Record<string, unknown>;
    expect(record).toMatchObject({ shadowRestoreState: 'failed', shadowActive: false });
    expect(Object.keys(record).some((key) => key.toLowerCase().includes('adoption'))).toBe(false);
    expect(detail).toEqual({
      idempotencyKey: 'upgrade-1:adopt-shadow',
      requestHash: '9'.repeat(64),
    });
    expect(statements.at(-1)?.text).toBe('COMMIT');
  });

  it('rolls back the state write when the append-only event fails', async () => {
    const pool = new FakePool(true);
    const store = new PostgresOperationStore(database, pool);
    const operation = fixtureOperation();
    await expect(
      store.insert(operation, operationEvent(operation, 'shadow_reserved')),
    ).rejects.toMatchObject({ code: 'database_busy', retryable: true });

    expect(pool.clients[0].statements.map(({ text }) => compact(text)).at(-1)).toBe('ROLLBACK');
    expect(pool.clients[0].statements.some(({ text }) => text === 'COMMIT')).toBe(false);
  });
});

class FakePool implements PostgresPool {
  readonly clients: FakeClient[] = [];

  constructor(private readonly failEvent = false) {}

  async connect(): Promise<PostgresClient> {
    const client = new FakeClient(this.failEvent);
    this.clients.push(client);
    return client;
  }

  async query(): Promise<QueryResult> {
    return result();
  }

  async end(): Promise<void> {}
}

class FakeClient implements PostgresClient {
  readonly statements: Array<{ text: string; values?: unknown[] }> = [];

  constructor(private readonly failEvent: boolean) {}

  async query(text: string, values?: unknown[]): Promise<QueryResult> {
    this.statements.push({ text, values });
    if (text.includes('SELECT version, name, checksum')) return result([]);
    if (this.failEvent && text.includes('INSERT INTO mizuki_deployment_events')) {
      throw Object.assign(new Error('statement timeout'), { code: '57014' });
    }
    return result([], 1);
  }

  release(): void {}
}

function result(rows: unknown[] = [], rowCount = 0): QueryResult {
  return {
    command: '',
    rowCount,
    oid: 0,
    fields: [],
    rows,
  } as QueryResult;
}

function fixtureOperation(): DeploymentOperation {
  const now = new Date('2026-08-23T12:00:00.000Z');
  return {
    upgradeId: 'upgrade-1',
    proposalId: 'proposal-1',
    repository: 'open-covenant/covenant',
    manifestSha256: 'e'.repeat(64),
    candidateSha: 'a'.repeat(40),
    artifactUrl: 'https://objects.githubusercontent.com/manifest.json',
    artifactSha256: 'f'.repeat(64),
    artifactSizeBytes: 100,
    imageRef: `ghcr.io/open-covenant/mizuki-api@sha256:${'f'.repeat(64)}`,
    artifactVerifiedAt: null,
    prNumber: 42,
    shadowIdempotencyKey: 'upgrade-1:shadow',
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

function compact(value: string): string {
  return value.replace(/\s+/g, ' ').trim();
}
