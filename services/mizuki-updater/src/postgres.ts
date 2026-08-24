import { Pool, type PoolClient, type QueryResultRow } from 'pg';
import {
  assertStateTransition,
  createAuditReceipt,
  sha256Hex,
  signedProposalSchema,
  type AuditEvent,
  type AuditReceipt,
  type NewUpgrade,
  type UpgradePatch,
  type UpgradeRecord,
  type UpgradeStats,
  UpdaterError,
} from './domain.js';
import type {
  PromotionControl,
  PromotionControlAuditEntry,
  PromotionFailureResolution,
  PromotionControlUpdate,
  PromotionReservation,
  UpgradeRepository,
} from './store.js';

interface SchemaMigration {
  version: number;
  name: string;
  sql: string;
}

const migrations: SchemaMigration[] = [
  {
    version: 1,
    name: 'initial_upgrade_state_machine',
    sql: `
      CREATE TABLE IF NOT EXISTS mizuki_upgrades (
        id uuid PRIMARY KEY,
        proposal_id text NOT NULL UNIQUE,
        idempotency_key text NOT NULL UNIQUE,
        request_hash text NOT NULL,
        envelope jsonb NOT NULL,
        state text NOT NULL CHECK (state IN (
          'submitted', 'verifying_artifact', 'proposal_verified', 'syncing_pr',
          'waiting_checks', 'starting_shadow', 'checking_shadow', 'merging',
          'promoting', 'completed', 'rollback_pending', 'rolled_back', 'failed',
          'rollback_failed'
        )),
        pr_number integer,
        pr_url text,
        deployment_id text,
        merge_sha text,
        wait_started_at timestamptz,
        next_attempt_at timestamptz,
        attempt_count integer NOT NULL DEFAULT 0,
        last_error_code text,
        last_error_message text,
        lease_owner text,
        lease_expires_at timestamptz,
        version integer NOT NULL DEFAULT 0,
        created_at timestamptz NOT NULL,
        updated_at timestamptz NOT NULL
      );
      CREATE INDEX IF NOT EXISTS mizuki_upgrades_runnable
        ON mizuki_upgrades (updated_at)
        WHERE state NOT IN ('completed', 'rolled_back', 'failed', 'rollback_failed');

      CREATE TABLE IF NOT EXISTS mizuki_upgrade_audit (
        id uuid PRIMARY KEY,
        upgrade_id uuid NOT NULL REFERENCES mizuki_upgrades(id),
        sequence integer NOT NULL,
        event text NOT NULL,
        from_state text,
        to_state text NOT NULL,
        details jsonb NOT NULL,
        occurred_at timestamptz NOT NULL,
        previous_hash text,
        hash text NOT NULL UNIQUE,
        UNIQUE (upgrade_id, sequence)
      );
      CREATE INDEX IF NOT EXISTS mizuki_upgrade_audit_upgrade
        ON mizuki_upgrade_audit (upgrade_id, sequence);
    `,
  },
  {
    version: 2,
    name: 'production_promotion_soak',
    sql: `
      ALTER TABLE mizuki_upgrades
        ADD COLUMN IF NOT EXISTS promotion_operation_id text;
      ALTER TABLE mizuki_upgrades
        ADD COLUMN IF NOT EXISTS promotion_healthy_at timestamptz;
      ALTER TABLE mizuki_upgrades
        DROP CONSTRAINT IF EXISTS mizuki_upgrades_state_check;
      ALTER TABLE mizuki_upgrades
        ADD CONSTRAINT mizuki_upgrades_state_check CHECK (state IN (
          'submitted', 'verifying_artifact', 'proposal_verified', 'syncing_pr',
          'waiting_checks', 'starting_shadow', 'checking_shadow', 'merging',
          'promoting', 'verifying_promotion', 'completed', 'rollback_pending',
          'rolled_back', 'failed', 'rollback_failed'
        ));
    `,
  },
  {
    version: 3,
    name: 'durable_promotion_control',
    sql: `
      CREATE TABLE mizuki_updater_promotion_control (
        singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
        promotions_enabled boolean NOT NULL,
        revision integer NOT NULL CHECK (revision >= 0),
        reason text NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 500),
        updated_by text NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 128),
        updated_at timestamptz NOT NULL
      );
      INSERT INTO mizuki_updater_promotion_control (
        singleton, promotions_enabled, revision, reason, updated_by, updated_at
      ) VALUES (
        true, false, 0, 'promotions are closed until explicitly enabled', 'system', now()
      );
    `,
  },
  {
    version: 4,
    name: 'crash_safe_promotion_reservations',
    sql: `
      ALTER TABLE mizuki_upgrades
        DROP CONSTRAINT IF EXISTS mizuki_upgrades_state_check;
      ALTER TABLE mizuki_upgrades
        ADD CONSTRAINT mizuki_upgrades_state_check CHECK (state IN (
          'submitted', 'verifying_artifact', 'proposal_verified', 'syncing_pr',
          'waiting_checks', 'starting_shadow', 'checking_shadow', 'merging',
          'merge_triggering', 'promoting', 'verifying_promotion', 'completed',
          'rollback_pending', 'rolled_back', 'failed', 'rollback_failed'
        ));
      ALTER TABLE mizuki_updater_promotion_control
        ADD COLUMN active_upgrade_id uuid REFERENCES mizuki_upgrades(id),
        ADD COLUMN active_since timestamptz,
        ADD CONSTRAINT mizuki_updater_active_promotion_pair CHECK (
          (active_upgrade_id IS NULL) = (active_since IS NULL)
        );
      CREATE TABLE mizuki_updater_promotion_control_audit (
        sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
        revision integer NOT NULL UNIQUE CHECK (revision >= 0),
        promotions_enabled boolean NOT NULL,
        reason text NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 500),
        updated_by text NOT NULL CHECK (char_length(updated_by) BETWEEN 1 AND 128),
        updated_at timestamptz NOT NULL,
        active_upgrade_id uuid,
        active_since timestamptz
      );
      INSERT INTO mizuki_updater_promotion_control_audit (
        revision, promotions_enabled, reason, updated_by, updated_at,
        active_upgrade_id, active_since
      )
      SELECT revision, promotions_enabled, reason, updated_by, updated_at,
             active_upgrade_id, active_since
      FROM mizuki_updater_promotion_control WHERE singleton = true;
      CREATE FUNCTION mizuki_reject_promotion_control_audit_mutation() RETURNS trigger
        LANGUAGE plpgsql AS $$
        BEGIN
          RAISE EXCEPTION 'promotion control audit is append-only';
        END;
        $$;
      CREATE TRIGGER mizuki_updater_promotion_control_audit_append_only
        BEFORE UPDATE OR DELETE ON mizuki_updater_promotion_control_audit
        FOR EACH ROW EXECUTE FUNCTION mizuki_reject_promotion_control_audit_mutation();
    `,
  },
];

const promotionAdmissionLock = 'mizuki-updater-promotion-admission';

export class PostgresUpgradeRepository implements UpgradeRepository {
  private readonly pool: Pool;

  constructor(connectionString: string) {
    this.pool = new Pool({
      connectionString,
      max: 10,
      statement_timeout: 15_000,
      idle_in_transaction_session_timeout: 15_000,
    });
  }

  async migrate(): Promise<void> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      await client.query("SELECT pg_advisory_xact_lock(hashtext('mizuki-updater-schema'))");
      await client.query(`
        CREATE TABLE IF NOT EXISTS mizuki_updater_migrations (
          version integer PRIMARY KEY CHECK (version > 0),
          name text NOT NULL UNIQUE,
          checksum text NOT NULL CHECK (checksum ~ '^[a-f0-9]{64}$'),
          applied_at timestamptz NOT NULL
        )
      `);
      const result = await client.query(
        'SELECT version, name, checksum FROM mizuki_updater_migrations ORDER BY version',
      );
      const applied = new Map<number, { name: string; checksum: string }>();
      for (const row of result.rows) {
        const version = Number(row.version);
        if (!migrations.some((migration) => migration.version === version)) {
          throw new UpdaterError(
            'database_migration_unknown',
            `Database has unknown migration ${version}`,
            500,
          );
        }
        applied.set(version, { name: String(row.name), checksum: String(row.checksum) });
      }

      for (const migration of migrations) {
        const checksum = sha256Hex(migration.sql);
        const existing = applied.get(migration.version);
        if (existing) {
          if (existing.name !== migration.name || existing.checksum !== checksum) {
            throw new UpdaterError(
              'database_migration_mismatch',
              `Database migration ${migration.version} does not match this build`,
              500,
            );
          }
          continue;
        }
        if ([...applied.keys()].some((version) => version > migration.version)) {
          throw new UpdaterError(
            'database_migration_gap',
            `Database migration ${migration.version} is missing`,
            500,
          );
        }

        await client.query(migration.sql);
        await client.query(
          `INSERT INTO mizuki_updater_migrations (version, name, checksum, applied_at)
           VALUES ($1, $2, $3, now())`,
          [migration.version, migration.name, checksum],
        );
        applied.set(migration.version, { name: migration.name, checksum });
      }
      await client.query('COMMIT');
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async promotionControl(): Promise<PromotionControl> {
    const result = await this.pool.query(
      `SELECT promotions_enabled, revision, reason, updated_by, updated_at,
              active_upgrade_id, active_since
       FROM mizuki_updater_promotion_control WHERE singleton = true`,
    );
    if (!result.rows[0]) {
      throw new UpdaterError(
        'promotion_control_unavailable',
        'Promotion control is unavailable',
        503,
      );
    }
    return toPromotionControl(result.rows[0]);
  }

  async promotionControlAudit(limit = 100): Promise<PromotionControlAuditEntry[]> {
    const safeLimit = Math.max(1, Math.min(500, Math.trunc(limit)));
    const result = await this.pool.query(
      `SELECT sequence, promotions_enabled, revision, reason, updated_by, updated_at,
              active_upgrade_id, active_since
       FROM mizuki_updater_promotion_control_audit ORDER BY sequence DESC LIMIT $1`,
      [safeLimit],
    );
    return result.rows.reverse().map((row) => ({
      ...toPromotionControl(row),
      sequence: Number(row.sequence),
    }));
  }

  async updatePromotionControl(
    input: PromotionControlUpdate,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionLock(async (client) => {
      await client.query('BEGIN');
      try {
        const current = await lockedPromotionControl(client);
        if (current.revision !== input.expectedRevision) {
          throw new UpdaterError(
            'promotion_control_conflict',
            'Promotion control changed concurrently',
            409,
          );
        }
        if (input.promotionsEnabled && current.activeUpgradeId) {
          const active = await client.query('SELECT state FROM mizuki_upgrades WHERE id = $1', [
            current.activeUpgradeId,
          ]);
          if (active.rows[0]?.state === 'rollback_failed') {
            throw new UpdaterError(
              'promotion_failure_unresolved',
              'The failed rollback must be explicitly resolved before promotions can be enabled',
              409,
            );
          }
        }
        const result = await client.query(
          `UPDATE mizuki_updater_promotion_control
           SET promotions_enabled = $1, revision = revision + 1, reason = $2,
               updated_by = $3, updated_at = $4
           WHERE singleton = true
           RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                     active_upgrade_id, active_since`,
          [input.promotionsEnabled, input.reason, input.updatedBy, now],
        );
        const control = toPromotionControl(result.rows[0]);
        await insertControlAudit(client, control);
        await client.query('COMMIT');
        return control;
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    });
  }

  async reservePromotion(upgradeId: string, now: Date): Promise<PromotionReservation> {
    return this.withPromotionLock(async (client) => {
      await client.query('BEGIN');
      try {
        let control = await lockedPromotionControl(client);
        if (control.activeUpgradeId) {
          const active = await client.query('SELECT state FROM mizuki_upgrades WHERE id = $1', [
            control.activeUpgradeId,
          ]);
          const state = active.rows[0]?.state as string | undefined;
          if (!state) {
            throw new UpdaterError(
              'promotion_reservation_invalid',
              'Promotion reservation references an unknown upgrade',
              500,
            );
          }
          if (state === 'rollback_failed') {
            if (control.promotionsEnabled) {
              const closed = await client.query(
                `UPDATE mizuki_updater_promotion_control
                 SET promotions_enabled = false, revision = revision + 1,
                     reason = $1, updated_by = $2, updated_at = $3
                 WHERE singleton = true
                 RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                           active_upgrade_id, active_since`,
                [
                  'promotion rollback requires operator intervention',
                  `updater:${control.activeUpgradeId}`,
                  now,
                ],
              );
              control = toPromotionControl(closed.rows[0]);
              await insertControlAudit(client, control);
            }
            await client.query('COMMIT');
            return { reserved: false, reason: 'disabled', control };
          }
          if (['completed', 'rolled_back', 'failed'].includes(state)) {
            const released = await client.query(
              `UPDATE mizuki_updater_promotion_control
               SET active_upgrade_id = NULL, active_since = NULL,
                   revision = revision + 1, reason = $1, updated_by = $2, updated_at = $3
               WHERE singleton = true
               RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                         active_upgrade_id, active_since`,
              [
                `terminal promotion reservation reconciled: ${state}`,
                `updater:${control.activeUpgradeId}`,
                now,
              ],
            );
            control = toPromotionControl(released.rows[0]);
            await insertControlAudit(client, control);
          } else if (control.activeUpgradeId !== upgradeId) {
            await client.query('COMMIT');
            return { reserved: false, reason: 'busy', control };
          }
        }
        if (!control.promotionsEnabled) {
          await client.query('COMMIT');
          return { reserved: false, reason: 'disabled', control };
        }
        if (!control.activeUpgradeId) {
          const reserved = await client.query(
            `UPDATE mizuki_updater_promotion_control
             SET active_upgrade_id = $1, active_since = $2,
                 revision = revision + 1, reason = $3, updated_by = $4, updated_at = $2
             WHERE singleton = true
             RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                       active_upgrade_id, active_since`,
            [upgradeId, now, 'promotion reservation acquired', `updater:${upgradeId}`],
          );
          control = toPromotionControl(reserved.rows[0]);
          await insertControlAudit(client, control);
        }
        await client.query('COMMIT');
        return { reserved: true, control };
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    });
  }

  async releasePromotion(upgradeId: string, now: Date): Promise<void> {
    await this.withPromotionLock(async (client) => {
      await client.query('BEGIN');
      try {
        const current = await lockedPromotionControl(client);
        if (current.activeUpgradeId !== upgradeId) {
          await client.query('COMMIT');
          return;
        }
        const active = await client.query('SELECT state FROM mizuki_upgrades WHERE id = $1', [
          upgradeId,
        ]);
        if (!['completed', 'rolled_back', 'failed'].includes(String(active.rows[0]?.state))) {
          throw new UpdaterError(
            'promotion_release_not_terminal',
            'Promotion reservation cannot be released before a terminal outcome',
            409,
          );
        }
        const result = await client.query(
          `UPDATE mizuki_updater_promotion_control
           SET active_upgrade_id = NULL, active_since = NULL,
               revision = revision + 1, reason = $1, updated_by = $2, updated_at = $3
           WHERE singleton = true
           RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                     active_upgrade_id, active_since`,
          ['promotion reservation released', `updater:${upgradeId}`, now],
        );
        await insertControlAudit(client, toPromotionControl(result.rows[0]));
        await client.query('COMMIT');
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    });
  }

  async closePromotionsForFailure(
    upgradeId: string,
    reason: string,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionLock(async (client) => {
      await client.query('BEGIN');
      try {
        const current = await lockedPromotionControl(client);
        if (current.activeUpgradeId && current.activeUpgradeId !== upgradeId) {
          throw new UpdaterError(
            'promotion_reservation_mismatch',
            'Another upgrade owns the promotion reservation',
            409,
          );
        }
        const result = await client.query(
          `UPDATE mizuki_updater_promotion_control
           SET promotions_enabled = false, revision = revision + 1, reason = $1,
               updated_by = $2, updated_at = $3, active_upgrade_id = $4,
               active_since = COALESCE(active_since, $3)
           WHERE singleton = true
           RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                     active_upgrade_id, active_since`,
          [reason, `updater:${upgradeId}`, now, upgradeId],
        );
        const control = toPromotionControl(result.rows[0]);
        await insertControlAudit(client, control);
        await client.query('COMMIT');
        return control;
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    });
  }

  async resolvePromotionFailure(
    input: PromotionFailureResolution,
    now: Date,
  ): Promise<PromotionControl> {
    return this.withPromotionLock(async (client) => {
      await client.query('BEGIN');
      try {
        const current = await lockedPromotionControl(client);
        if (current.revision !== input.expectedRevision) {
          throw new UpdaterError(
            'promotion_control_conflict',
            'Promotion control changed concurrently',
            409,
          );
        }
        if (current.promotionsEnabled || current.activeUpgradeId !== input.upgradeId) {
          throw new UpdaterError(
            'promotion_failure_resolution_invalid',
            'The upgrade does not own an unresolved failed rollback',
            409,
          );
        }
        const active = await client.query('SELECT state FROM mizuki_upgrades WHERE id = $1', [
          input.upgradeId,
        ]);
        if (active.rows[0]?.state !== 'rollback_failed') {
          throw new UpdaterError(
            'promotion_failure_resolution_invalid',
            'The upgrade does not own an unresolved failed rollback',
            409,
          );
        }
        const result = await client.query(
          `UPDATE mizuki_updater_promotion_control
           SET active_upgrade_id = NULL, active_since = NULL,
               revision = revision + 1, reason = $1, updated_by = $2, updated_at = $3
           WHERE singleton = true
           RETURNING promotions_enabled, revision, reason, updated_by, updated_at,
                     active_upgrade_id, active_since`,
          [input.reason, input.updatedBy, now],
        );
        const control = toPromotionControl(result.rows[0]);
        await insertControlAudit(client, control);
        await client.query('COMMIT');
        return control;
      } catch (error) {
        await client.query('ROLLBACK');
        throw error;
      }
    });
  }

  async reserve(input: NewUpgrade, now: Date): Promise<UpgradeRecord> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const lockKeys = [
        `mizuki-updater:idempotency:${input.idempotencyKey}`,
        `mizuki-updater:proposal:${input.proposalId}`,
      ].sort();
      for (const key of lockKeys) {
        await client.query('SELECT pg_advisory_xact_lock(hashtext($1))', [key]);
      }
      const existing = await client.query(
        `SELECT * FROM mizuki_upgrades
         WHERE idempotency_key = $1 OR proposal_id = $2
         FOR UPDATE`,
        [input.idempotencyKey, input.proposalId],
      );
      if (existing.rows[0]) {
        const record = toRecord(existing.rows[0]);
        if (record.requestHash !== input.requestHash) {
          throw new UpdaterError(
            'idempotency_conflict',
            'Proposal or idempotency key was already used for different content',
            409,
          );
        }
        await client.query('COMMIT');
        return record;
      }

      const record: UpgradeRecord = {
        ...input,
        state: 'submitted',
        prNumber: null,
        prUrl: null,
        deploymentId: null,
        mergeSha: null,
        promotionOperationId: null,
        promotionHealthyAt: null,
        waitStartedAt: null,
        nextAttemptAt: null,
        attemptCount: 0,
        lastErrorCode: null,
        lastErrorMessage: null,
        leaseOwner: null,
        leaseExpiresAt: null,
        version: 0,
        createdAt: now,
        updatedAt: now,
      };
      await client.query(
        `INSERT INTO mizuki_upgrades (
          id, proposal_id, idempotency_key, request_hash, envelope, state,
          attempt_count, version, created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, 0, 0, $7, $7)`,
        [
          record.id,
          record.proposalId,
          record.idempotencyKey,
          record.requestHash,
          JSON.stringify(record.envelope),
          record.state,
          now,
        ],
      );
      const receipt = createAuditReceipt(
        record.id,
        1,
        null,
        'submitted',
        { event: 'proposal_submitted', details: { manifestSha256: input.envelope.manifestSha256 } },
        now,
        null,
      );
      await insertReceipt(client, receipt);
      await client.query('COMMIT');
      return record;
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async get(id: string): Promise<UpgradeRecord | null> {
    const result = await this.pool.query('SELECT * FROM mizuki_upgrades WHERE id = $1', [id]);
    return result.rows[0] ? toRecord(result.rows[0]) : null;
  }

  async getByProposalId(proposalId: string): Promise<UpgradeRecord | null> {
    const result = await this.pool.query('SELECT * FROM mizuki_upgrades WHERE proposal_id = $1', [
      proposalId,
    ]);
    return result.rows[0] ? toRecord(result.rows[0]) : null;
  }

  async audit(id: string): Promise<AuditReceipt[]> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_upgrade_audit WHERE upgrade_id = $1 ORDER BY sequence',
      [id],
    );
    return result.rows.map(toReceipt);
  }

  async acquireLease(id: string, owner: string, now: Date, leaseMs: number): Promise<boolean> {
    const expiresAt = new Date(now.getTime() + leaseMs);
    const result = await this.pool.query(
      `UPDATE mizuki_upgrades
       SET lease_owner = $2, lease_expires_at = $3
       WHERE id = $1
         AND state NOT IN ('completed', 'rolled_back', 'failed', 'rollback_failed')
         AND (lease_expires_at IS NULL OR lease_expires_at <= $4 OR lease_owner = $2)
       RETURNING id`,
      [id, owner, expiresAt, now],
    );
    return result.rowCount === 1;
  }

  async releaseLease(id: string, owner: string): Promise<void> {
    await this.pool.query(
      `UPDATE mizuki_upgrades
       SET lease_owner = NULL, lease_expires_at = NULL
       WHERE id = $1 AND lease_owner = $2`,
      [id, owner],
    );
  }

  async transition(
    id: string,
    expectedVersion: number,
    leaseOwner: string,
    patch: UpgradePatch,
    event: AuditEvent,
    now: Date,
  ): Promise<UpgradeRecord> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const selected = await client.query(
        'SELECT * FROM mizuki_upgrades WHERE id = $1 FOR UPDATE',
        [id],
      );
      if (!selected.rows[0])
        throw new UpdaterError('upgrade_not_found', 'Upgrade was not found', 404);
      const current = toRecord(selected.rows[0]);
      if (current.leaseOwner !== leaseOwner) {
        throw new UpdaterError('lease_lost', 'Upgrade lease is not held', 409, true);
      }
      if (current.version !== expectedVersion) {
        throw new UpdaterError('version_conflict', 'Upgrade changed concurrently', 409, true);
      }
      assertStateTransition(current.state, patch.state ?? current.state);

      const columns: string[] = [];
      const values: unknown[] = [];
      const mappings: Array<[keyof UpgradePatch, string]> = [
        ['state', 'state'],
        ['prNumber', 'pr_number'],
        ['prUrl', 'pr_url'],
        ['deploymentId', 'deployment_id'],
        ['mergeSha', 'merge_sha'],
        ['promotionOperationId', 'promotion_operation_id'],
        ['promotionHealthyAt', 'promotion_healthy_at'],
        ['waitStartedAt', 'wait_started_at'],
        ['nextAttemptAt', 'next_attempt_at'],
        ['attemptCount', 'attempt_count'],
        ['lastErrorCode', 'last_error_code'],
        ['lastErrorMessage', 'last_error_message'],
      ];
      for (const [key, column] of mappings) {
        if (!(key in patch)) continue;
        values.push(patch[key]);
        columns.push(`${column} = $${values.length}`);
      }
      values.push(now, id, expectedVersion, leaseOwner);
      columns.push(`updated_at = $${values.length - 3}`, 'version = version + 1');
      const updated = await client.query(
        `UPDATE mizuki_upgrades SET ${columns.join(', ')}
         WHERE id = $${values.length - 2}
           AND version = $${values.length - 1}
           AND lease_owner = $${values.length}
         RETURNING *`,
        values,
      );
      if (!updated.rows[0])
        throw new UpdaterError('version_conflict', 'Upgrade changed concurrently', 409, true);
      const record = toRecord(updated.rows[0]);

      const prior = await client.query(
        `SELECT sequence, hash FROM mizuki_upgrade_audit
         WHERE upgrade_id = $1 ORDER BY sequence DESC LIMIT 1`,
        [id],
      );
      const previous = prior.rows[0] as { sequence: number; hash: string } | undefined;
      const receipt = createAuditReceipt(
        id,
        (previous?.sequence ?? 0) + 1,
        current.state,
        record.state,
        event,
        now,
        previous?.hash ?? null,
      );
      await insertReceipt(client, receipt);
      await client.query('COMMIT');
      return record;
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async listRunnable(now: Date, limit: number): Promise<string[]> {
    const result = await this.pool.query(
      `SELECT id FROM mizuki_upgrades
       WHERE state NOT IN ('completed', 'rolled_back', 'failed', 'rollback_failed')
         AND (next_attempt_at IS NULL OR next_attempt_at <= $1)
         AND (lease_expires_at IS NULL OR lease_expires_at <= $1)
       ORDER BY updated_at
       LIMIT $2`,
      [now, limit],
    );
    return result.rows.map((row) => String(row.id));
  }

  async stats(): Promise<UpgradeStats> {
    const result = await this.pool.query(
      'SELECT state, count(*)::integer AS count FROM mizuki_upgrades GROUP BY state',
    );
    const byState: UpgradeStats['byState'] = {};
    let total = 0;
    for (const row of result.rows) {
      const count = Number(row.count);
      byState[row.state as keyof typeof byState] = count;
      total += count;
    }
    return { total, byState };
  }

  async health(): Promise<void> {
    await this.promotionControl();
  }

  async close(): Promise<void> {
    await this.pool.end();
  }

  private async withPromotionLock<T>(action: (client: PoolClient) => Promise<T>): Promise<T> {
    const client = await this.pool.connect();
    let destroy = false;
    try {
      await client.query('SELECT pg_advisory_lock(hashtext($1))', [promotionAdmissionLock]);
      return await action(client);
    } finally {
      try {
        await client.query('SELECT pg_advisory_unlock(hashtext($1))', [promotionAdmissionLock]);
      } catch {
        destroy = true;
      }
      client.release(destroy);
    }
  }
}

async function insertReceipt(client: PoolClient, receipt: AuditReceipt): Promise<void> {
  await client.query(
    `INSERT INTO mizuki_upgrade_audit (
      id, upgrade_id, sequence, event, from_state, to_state, details,
      occurred_at, previous_hash, hash
    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
    [
      receipt.id,
      receipt.upgradeId,
      receipt.sequence,
      receipt.event,
      receipt.fromState,
      receipt.toState,
      JSON.stringify(receipt.details),
      receipt.occurredAt,
      receipt.previousHash,
      receipt.hash,
    ],
  );
}

async function lockedPromotionControl(client: PoolClient): Promise<PromotionControl> {
  const result = await client.query(
    `SELECT promotions_enabled, revision, reason, updated_by, updated_at,
            active_upgrade_id, active_since
     FROM mizuki_updater_promotion_control WHERE singleton = true FOR UPDATE`,
  );
  if (!result.rows[0]) {
    throw new UpdaterError(
      'promotion_control_unavailable',
      'Promotion control is unavailable',
      503,
    );
  }
  return toPromotionControl(result.rows[0]);
}

async function insertControlAudit(client: PoolClient, control: PromotionControl): Promise<void> {
  await client.query(
    `INSERT INTO mizuki_updater_promotion_control_audit (
       revision, promotions_enabled, reason, updated_by, updated_at,
       active_upgrade_id, active_since
     ) VALUES ($1, $2, $3, $4, $5, $6, $7)`,
    [
      control.revision,
      control.promotionsEnabled,
      control.reason,
      control.updatedBy,
      control.updatedAt,
      control.activeUpgradeId,
      control.activeSince,
    ],
  );
}

function toRecord(row: QueryResultRow): UpgradeRecord {
  return {
    id: String(row.id),
    proposalId: String(row.proposal_id),
    idempotencyKey: String(row.idempotency_key),
    requestHash: String(row.request_hash),
    envelope: signedProposalSchema.parse(row.envelope),
    state: row.state,
    prNumber: row.pr_number === null ? null : Number(row.pr_number),
    prUrl: row.pr_url,
    deploymentId: row.deployment_id,
    mergeSha: row.merge_sha,
    promotionOperationId: row.promotion_operation_id,
    promotionHealthyAt: row.promotion_healthy_at ? new Date(row.promotion_healthy_at) : null,
    waitStartedAt: row.wait_started_at ? new Date(row.wait_started_at) : null,
    nextAttemptAt: row.next_attempt_at ? new Date(row.next_attempt_at) : null,
    attemptCount: Number(row.attempt_count),
    lastErrorCode: row.last_error_code,
    lastErrorMessage: row.last_error_message,
    leaseOwner: row.lease_owner,
    leaseExpiresAt: row.lease_expires_at ? new Date(row.lease_expires_at) : null,
    version: Number(row.version),
    createdAt: new Date(row.created_at),
    updatedAt: new Date(row.updated_at),
  };
}

function toReceipt(row: QueryResultRow): AuditReceipt {
  return {
    id: String(row.id),
    upgradeId: String(row.upgrade_id),
    sequence: Number(row.sequence),
    event: String(row.event),
    fromState: row.from_state,
    toState: row.to_state,
    details: row.details as Record<string, unknown>,
    occurredAt: new Date(row.occurred_at),
    previousHash: row.previous_hash,
    hash: String(row.hash),
  };
}

function toPromotionControl(row: QueryResultRow): PromotionControl {
  return {
    promotionsEnabled: Boolean(row.promotions_enabled),
    revision: Number(row.revision),
    reason: String(row.reason),
    updatedBy: String(row.updated_by),
    updatedAt: new Date(row.updated_at),
    activeUpgradeId: row.active_upgrade_id === null ? null : String(row.active_upgrade_id),
    activeSince: row.active_since ? new Date(row.active_since) : null,
  };
}
