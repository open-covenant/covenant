import { createHash } from 'node:crypto';
import { Pool, type PoolConfig, type QueryResult } from 'pg';
import { z } from 'zod';
import type { DatabaseConfig } from './config.js';
import {
  ControllerError,
  externalId,
  gitSha,
  sha256,
  type DeploymentOperation,
  type OperationEvent,
} from './domain.js';

export interface OperationStore {
  migrate(): Promise<void>;
  withMutationLock<T>(action: () => Promise<T>): Promise<T>;
  insert(operation: DeploymentOperation, event: OperationEvent): Promise<void>;
  save(operation: DeploymentOperation, event: OperationEvent): Promise<void>;
  get(upgradeId: string): Promise<DeploymentOperation | null>;
  getByProposal(proposalId: string): Promise<DeploymentOperation | null>;
  getByShadowDeploy(deployId: string): Promise<DeploymentOperation | null>;
  getByIdempotency(key: string): Promise<DeploymentOperation | null>;
  activeShadow(): Promise<DeploymentOperation | null>;
  activeProduction(): Promise<DeploymentOperation | null>;
  events(upgradeId: string): Promise<OperationEvent[]>;
  health(): Promise<void>;
  close(): Promise<void>;
}

const actionState = z.enum(['reserved', 'triggering', 'triggered', 'completed', 'failed']);
const nullableActionState = actionState.nullable();
const nullableExternalId = externalId.nullable();
const nullableGitSha = gitSha.nullable();
const nullableSha256 = sha256.nullable();
const nullableDate = z.coerce.date().nullable();
const imageRef = z
  .string()
  .regex(/^[a-z0-9.-]+(?::[0-9]{1,5})?\/[a-z0-9._/-]+@sha256:[a-f0-9]{64}$/);
const operationSchema = z
  .object({
    upgradeId: externalId,
    proposalId: externalId,
    repository: z.string().min(3).max(201),
    manifestSha256: sha256,
    candidateSha: gitSha,
    artifactUrl: z.string().url(),
    artifactSha256: sha256,
    artifactSizeBytes: z.number().int().positive(),
    imageRef,
    artifactVerifiedAt: nullableDate,
    prNumber: z.number().int().positive(),
    shadowIdempotencyKey: externalId,
    shadowRequestHash: sha256,
    shadowState: actionState,
    shadowServiceFingerprint: sha256,
    shadowBaselineDeployId: externalId,
    shadowBaselineArtifactSha256: sha256,
    shadowStartedAt: nullableDate,
    shadowDeployId: nullableExternalId,
    shadowActive: z.boolean(),
    shadowRestoreState: nullableActionState,
    shadowRestoreStartedAt: nullableDate,
    shadowRestoreDeployId: nullableExternalId,
    promotionIdempotencyKey: nullableExternalId,
    promotionRequestHash: nullableSha256,
    promotionState: nullableActionState,
    mergeSha: nullableGitSha,
    productionServiceFingerprint: nullableSha256,
    productionBaselineDeployId: nullableExternalId,
    productionBaselineArtifactSha256: nullableSha256,
    promotionStartedAt: nullableDate,
    promotionDeployId: nullableExternalId,
    productionActive: z.boolean(),
    productionFinalizedAt: nullableDate.default(null),
    rollbackIdempotencyKey: nullableExternalId,
    rollbackRequestHash: nullableSha256,
    rollbackState: nullableActionState,
    rollbackStartedAt: nullableDate,
    rollbackDeployId: nullableExternalId,
    createdAt: z.coerce.date(),
    updatedAt: z.coerce.date(),
  })
  .strict();
const eventSchema = z
  .object({
    operationId: externalId,
    type: z
      .string()
      .min(1)
      .max(100)
      .regex(/^[a-z][a-z0-9_]*$/),
    recordSha256: sha256,
    detail: z.record(z.string(), z.union([z.string(), z.number(), z.boolean(), z.null()])),
    createdAt: z.coerce.date(),
  })
  .strict();

const migration1 = `
  CREATE TABLE mizuki_deployment_operations (
    upgrade_id text PRIMARY KEY,
    proposal_id text NOT NULL UNIQUE,
    shadow_idempotency_key text NOT NULL UNIQUE,
    shadow_deploy_id text UNIQUE,
    shadow_active boolean NOT NULL,
    promotion_idempotency_key text UNIQUE,
    promotion_deploy_id text UNIQUE,
    production_active boolean NOT NULL,
    rollback_idempotency_key text UNIQUE,
    record jsonb NOT NULL,
    updated_at timestamptz NOT NULL
  );
  CREATE UNIQUE INDEX mizuki_deployment_one_shadow
    ON mizuki_deployment_operations ((true)) WHERE shadow_active;
  CREATE UNIQUE INDEX mizuki_deployment_one_production
    ON mizuki_deployment_operations ((true)) WHERE production_active;
`;

const migration2 = `
  CREATE TABLE mizuki_deployment_events (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    operation_id text NOT NULL REFERENCES mizuki_deployment_operations(upgrade_id),
    event_type text NOT NULL CHECK (event_type ~ '^[a-z][a-z0-9_]*$'),
    record_sha256 text NOT NULL CHECK (record_sha256 ~ '^[a-f0-9]{64}$'),
    detail jsonb NOT NULL,
    created_at timestamptz NOT NULL
  );
  CREATE INDEX mizuki_deployment_events_operation
    ON mizuki_deployment_events (operation_id, sequence);
  CREATE FUNCTION mizuki_reject_event_mutation() RETURNS trigger
    LANGUAGE plpgsql AS $$
    BEGIN
      RAISE EXCEPTION 'deployment events are append-only';
    END;
    $$;
  CREATE TRIGGER mizuki_deployment_events_append_only
    BEFORE UPDATE OR DELETE ON mizuki_deployment_events
    FOR EACH ROW EXECUTE FUNCTION mizuki_reject_event_mutation();
`;

const migrations = [
  { version: 1, name: 'initial_operation_ledger', sql: migration1 },
  { version: 2, name: 'append_only_operation_events', sql: migration2 },
];

export interface PostgresClient {
  query(text: string, values?: unknown[]): Promise<QueryResult>;
  release(): void;
}

export interface PostgresPool {
  connect(): Promise<PostgresClient>;
  query(text: string, values?: unknown[]): Promise<QueryResult>;
  end(): Promise<void>;
}

export class PostgresOperationStore implements OperationStore {
  private readonly pool: PostgresPool;

  constructor(config: DatabaseConfig, pool?: PostgresPool) {
    this.pool = pool ?? new Pool(postgresPoolConfig(config));
  }

  async migrate(): Promise<void> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      await client.query("SELECT pg_advisory_xact_lock(hashtext('mizuki-deployment-schema'))");
      await client.query(`
        CREATE TABLE IF NOT EXISTS mizuki_deployment_migrations (
          version integer PRIMARY KEY,
          name text NOT NULL UNIQUE,
          checksum text NOT NULL CHECK (checksum ~ '^[a-f0-9]{64}$'),
          applied_at timestamptz NOT NULL
        )
      `);
      const applied = await client.query(
        'SELECT version, name, checksum FROM mizuki_deployment_migrations ORDER BY version',
      );
      for (const row of applied.rows) {
        const migration = migrations.find((value) => value.version === Number(row.version));
        if (!migration) {
          throw new ControllerError(
            'database_migration_unknown',
            'Unknown database migration',
            500,
          );
        }
        const checksum = createHash('sha256').update(migration.sql).digest('hex');
        if (row.name !== migration.name || row.checksum !== checksum) {
          throw new ControllerError(
            'database_migration_mismatch',
            'Database migration does not match this build',
            500,
          );
        }
      }
      if (applied.rows.length > 0) {
        const versions = applied.rows.map((row) => Number(row.version));
        if (versions.some((version, index) => version !== index + 1)) {
          throw new ControllerError(
            'database_migration_gap',
            'Database migrations have a gap',
            500,
          );
        }
      }
      for (const migration of migrations.slice(applied.rows.length)) {
        await client.query(migration.sql);
        await client.query(
          `INSERT INTO mizuki_deployment_migrations (version, name, checksum, applied_at)
           VALUES ($1, $2, $3, now())`,
          [
            migration.version,
            migration.name,
            createHash('sha256').update(migration.sql).digest('hex'),
          ],
        );
      }
      await client.query('COMMIT');
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async withMutationLock<T>(action: () => Promise<T>): Promise<T> {
    const client = await this.pool.connect();
    let locked = false;
    try {
      const result = await client.query(
        "SELECT pg_try_advisory_lock(hashtext('mizuki-deployment-mutations')) AS locked",
      );
      locked = result.rows[0]?.locked === true;
      if (!locked) {
        throw new ControllerError(
          'mutation_busy',
          'Another deployment operation is in progress',
          503,
          true,
          2,
        );
      }
      return await action();
    } finally {
      if (locked) {
        await client.query("SELECT pg_advisory_unlock(hashtext('mizuki-deployment-mutations'))");
      }
      client.release();
    }
  }

  async insert(operation: DeploymentOperation, event: OperationEvent): Promise<void> {
    const value = operationSchema.parse(operation);
    const audit = eventSchema.parse(event);
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      await client.query(
        `INSERT INTO mizuki_deployment_operations (
           upgrade_id, proposal_id, shadow_idempotency_key, shadow_deploy_id,
           shadow_active, promotion_idempotency_key, promotion_deploy_id,
           production_active, rollback_idempotency_key, record, updated_at
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)`,
        columns(value),
      );
      await append(client, audit);
      await client.query('COMMIT');
    } catch (error) {
      await client.query('ROLLBACK');
      throw databaseError(error);
    } finally {
      client.release();
    }
  }

  async save(operation: DeploymentOperation, event: OperationEvent): Promise<void> {
    const value = operationSchema.parse(operation);
    const audit = eventSchema.parse(event);
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const result = await client.query(
        `UPDATE mizuki_deployment_operations SET
           proposal_id=$2, shadow_idempotency_key=$3, shadow_deploy_id=$4,
           shadow_active=$5, promotion_idempotency_key=$6, promotion_deploy_id=$7,
           production_active=$8, rollback_idempotency_key=$9, record=$10, updated_at=$11
         WHERE upgrade_id=$1`,
        columns(value),
      );
      if (result.rowCount !== 1) {
        throw new ControllerError('operation_not_found', 'Deployment operation was not found', 404);
      }
      await append(client, audit);
      await client.query('COMMIT');
    } catch (error) {
      await client.query('ROLLBACK');
      throw databaseError(error);
    } finally {
      client.release();
    }
  }

  get(upgradeId: string): Promise<DeploymentOperation | null> {
    return this.one('upgrade_id = $1', upgradeId);
  }

  getByProposal(proposalId: string): Promise<DeploymentOperation | null> {
    return this.one('proposal_id = $1', proposalId);
  }

  getByShadowDeploy(deployId: string): Promise<DeploymentOperation | null> {
    return this.one('shadow_deploy_id = $1', deployId);
  }

  getByIdempotency(key: string): Promise<DeploymentOperation | null> {
    return this.one(
      'shadow_idempotency_key = $1 OR promotion_idempotency_key = $1 OR rollback_idempotency_key = $1',
      key,
    );
  }

  activeShadow(): Promise<DeploymentOperation | null> {
    return this.one('shadow_active = true');
  }

  activeProduction(): Promise<DeploymentOperation | null> {
    return this.one('production_active = true');
  }

  async events(upgradeId: string): Promise<OperationEvent[]> {
    const result = await this.pool.query(
      `SELECT operation_id, event_type, record_sha256, detail, created_at
       FROM mizuki_deployment_events WHERE operation_id = $1 ORDER BY sequence`,
      [upgradeId],
    );
    return result.rows.map((row) =>
      eventSchema.parse({
        operationId: row.operation_id,
        type: row.event_type,
        recordSha256: row.record_sha256,
        detail: row.detail,
        createdAt: row.created_at,
      }),
    );
  }

  async health(): Promise<void> {
    await this.pool.query('SELECT 1');
  }

  async close(): Promise<void> {
    await this.pool.end();
  }

  private async one(where: string, value?: string): Promise<DeploymentOperation | null> {
    const result = await this.pool.query(
      `SELECT record FROM mizuki_deployment_operations WHERE ${where} LIMIT 1`,
      value === undefined ? [] : [value],
    );
    return result.rows[0] ? operationSchema.parse(result.rows[0].record) : null;
  }
}

export class MemoryOperationStore implements OperationStore {
  private readonly operations = new Map<string, DeploymentOperation>();
  private readonly audit = new Map<string, OperationEvent[]>();
  private gate = Promise.resolve();

  async migrate(): Promise<void> {}

  async withMutationLock<T>(action: () => Promise<T>): Promise<T> {
    let release = () => {};
    const previous = this.gate;
    this.gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await previous;
    try {
      return await action();
    } finally {
      release();
    }
  }

  async insert(operation: DeploymentOperation, event: OperationEvent): Promise<void> {
    const value = operationSchema.parse(operation);
    const audit = eventSchema.parse(event);
    if (
      (await this.get(value.upgradeId)) ||
      (await this.getByProposal(value.proposalId)) ||
      (await this.getByIdempotency(value.shadowIdempotencyKey))
    ) {
      throw new ControllerError('operation_conflict', 'Deployment operation already exists', 409);
    }
    this.assertSlots(value);
    this.operations.set(value.upgradeId, structuredClone(value));
    this.append(audit);
  }

  async save(operation: DeploymentOperation, event: OperationEvent): Promise<void> {
    const value = operationSchema.parse(operation);
    const audit = eventSchema.parse(event);
    if (!this.operations.has(value.upgradeId)) {
      throw new ControllerError('operation_not_found', 'Deployment operation was not found', 404);
    }
    this.assertSlots(value);
    this.operations.set(value.upgradeId, structuredClone(value));
    this.append(audit);
  }

  async get(upgradeId: string): Promise<DeploymentOperation | null> {
    return clone(this.operations.get(upgradeId));
  }

  async getByProposal(proposalId: string): Promise<DeploymentOperation | null> {
    return clone([...this.operations.values()].find((value) => value.proposalId === proposalId));
  }

  async getByShadowDeploy(deployId: string): Promise<DeploymentOperation | null> {
    return clone([...this.operations.values()].find((value) => value.shadowDeployId === deployId));
  }

  async getByIdempotency(key: string): Promise<DeploymentOperation | null> {
    return clone(
      [...this.operations.values()].find(
        (value) =>
          value.shadowIdempotencyKey === key ||
          value.promotionIdempotencyKey === key ||
          value.rollbackIdempotencyKey === key,
      ),
    );
  }

  async activeShadow(): Promise<DeploymentOperation | null> {
    return clone([...this.operations.values()].find((value) => value.shadowActive));
  }

  async activeProduction(): Promise<DeploymentOperation | null> {
    return clone([...this.operations.values()].find((value) => value.productionActive));
  }

  async events(upgradeId: string): Promise<OperationEvent[]> {
    return structuredClone(this.audit.get(upgradeId) ?? []);
  }

  async health(): Promise<void> {}
  async close(): Promise<void> {}

  private append(event: OperationEvent): void {
    const events = this.audit.get(event.operationId) ?? [];
    events.push(structuredClone(event));
    this.audit.set(event.operationId, events);
  }

  private assertSlots(operation: DeploymentOperation): void {
    for (const current of this.operations.values()) {
      if (current.upgradeId === operation.upgradeId) continue;
      if (current.shadowActive && operation.shadowActive) {
        throw new ControllerError(
          'shadow_busy',
          'The shadow service is already reserved',
          503,
          true,
          5,
        );
      }
      if (current.productionActive && operation.productionActive) {
        throw new ControllerError(
          'production_busy',
          'A production promotion is still active',
          503,
          true,
          5,
        );
      }
    }
  }
}

async function append(client: PostgresClient, event: OperationEvent): Promise<void> {
  await client.query(
    `INSERT INTO mizuki_deployment_events
       (operation_id, event_type, record_sha256, detail, created_at)
     VALUES ($1, $2, $3, $4, $5)`,
    [
      event.operationId,
      event.type,
      event.recordSha256,
      JSON.stringify(event.detail),
      event.createdAt,
    ],
  );
}

export function postgresPoolConfig(config: DatabaseConfig): PoolConfig {
  return {
    connectionString: config.connectionString,
    max: config.maxConnections,
    connectionTimeoutMillis: config.connectionTimeoutMs,
    statement_timeout: 15_000,
    idle_in_transaction_session_timeout: 15_000,
    keepAlive: true,
    ssl:
      config.sslMode === 'disable'
        ? false
        : { rejectUnauthorized: config.sslMode === 'verify-full' },
  };
}

function columns(value: DeploymentOperation): unknown[] {
  return [
    value.upgradeId,
    value.proposalId,
    value.shadowIdempotencyKey,
    value.shadowDeployId,
    value.shadowActive,
    value.promotionIdempotencyKey,
    value.promotionDeployId,
    value.productionActive,
    value.rollbackIdempotencyKey,
    JSON.stringify(value),
    value.updatedAt,
  ];
}

function databaseError(error: unknown): Error {
  if (error instanceof ControllerError) return error;
  if (error && typeof error === 'object' && 'code' in error) {
    if (error.code === '23505') {
      return new ControllerError('operation_conflict', 'Deployment operation conflicts', 409);
    }
    if (error.code === '55P03' || error.code === '57014') {
      return new ControllerError('database_busy', 'Deployment database is busy', 503, true, 2);
    }
  }
  return new ControllerError('database_unavailable', 'Deployment database failed', 503, true, 5);
}

function clone(value: DeploymentOperation | undefined): DeploymentOperation | null {
  return value ? structuredClone(value) : null;
}
