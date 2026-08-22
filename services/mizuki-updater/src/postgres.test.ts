import { randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { PostgresUpgradeRepository } from './postgres.js';

const databaseUrl = process.env.MIZUKI_UPDATER_TEST_DATABASE_URL;

describe.skipIf(!databaseUrl)('Postgres schema migrations', () => {
  const schema = `mizuki_updater_${randomUUID().replaceAll('-', '')}`;
  let admin: Pool;
  let scopedUrl: string;

  beforeAll(async () => {
    admin = new Pool({ connectionString: databaseUrl });
    await admin.query(`CREATE SCHEMA "${schema}"`);
    const url = new URL(databaseUrl!);
    url.searchParams.set('options', `-c search_path=${schema}`);
    scopedUrl = url.toString();
  });

  afterAll(async () => {
    await admin.query(`DROP SCHEMA IF EXISTS "${schema}" CASCADE`);
    await admin.end();
  });

  it('serializes, upgrades, and idempotently replays the migration history', async () => {
    const legacy = new Pool({ connectionString: scopedUrl });
    await legacy.query(legacySchema);
    await legacy.end();

    const first = new PostgresUpgradeRepository(scopedUrl);
    const second = new PostgresUpgradeRepository(scopedUrl);
    try {
      await Promise.all([first.migrate(), second.migrate()]);
      await first.migrate();

      const inspection = new Pool({ connectionString: scopedUrl });
      try {
        const history = await inspection.query(
          'SELECT version, name, checksum FROM mizuki_updater_migrations ORDER BY version',
        );
        expect(history.rows).toMatchObject([
          { version: 1, name: 'initial_upgrade_state_machine' },
          { version: 2, name: 'production_promotion_soak' },
          { version: 3, name: 'durable_promotion_control' },
        ]);
        expect(history.rows.every((row) => /^[a-f0-9]{64}$/.test(String(row.checksum)))).toBe(true);

        const columns = await inspection.query(
          `SELECT column_name FROM information_schema.columns
           WHERE table_schema = $1 AND table_name = 'mizuki_upgrades'
             AND column_name IN ('promotion_operation_id', 'promotion_healthy_at')
           ORDER BY column_name`,
          [schema],
        );
        expect(columns.rows.map((row) => row.column_name)).toEqual([
          'promotion_healthy_at',
          'promotion_operation_id',
        ]);

        const constraint = await inspection.query(
          `SELECT pg_get_constraintdef(oid) AS definition
             FROM pg_constraint
            WHERE conrelid = 'mizuki_upgrades'::regclass
              AND conname = 'mizuki_upgrades_state_check'`,
        );
        expect(constraint.rows[0]?.definition).toContain('verifying_promotion');

        const control = await inspection.query(
          `SELECT promotions_enabled, revision, reason, updated_by
           FROM mizuki_updater_promotion_control WHERE singleton = true`,
        );
        expect(control.rows).toEqual([
          {
            promotions_enabled: false,
            revision: 0,
            reason: 'promotions are closed until explicitly enabled',
            updated_by: 'system',
          },
        ]);

        await expect(
          inspection.query(
            `INSERT INTO mizuki_upgrades (
              id, proposal_id, idempotency_key, request_hash, envelope, state,
              promotion_operation_id, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, 'verifying_promotion', $6, now(), now())`,
            [
              randomUUID(),
              'migration-proposal',
              'migration-key',
              'request-hash',
              {},
              'promotion-1',
            ],
          ),
        ).resolves.toMatchObject({ rowCount: 1 });
      } finally {
        await inspection.end();
      }
    } finally {
      await first.close();
      await second.close();
    }
  });

  it('persists promotion control and serializes pause against admission', async () => {
    const first = new PostgresUpgradeRepository(scopedUrl);
    const second = new PostgresUpgradeRepository(scopedUrl);
    await Promise.all([first.migrate(), second.migrate()]);
    try {
      const initial = await first.promotionControl();
      expect(initial).toMatchObject({ promotionsEnabled: false, revision: 0 });
      await first.updatePromotionControl(
        {
          promotionsEnabled: true,
          expectedRevision: 0,
          reason: 'approved controlled promotion',
          updatedBy: 'write_authority',
        },
        new Date('2026-08-22T12:00:00.000Z'),
      );

      const actionStarted = deferred<void>();
      const finishAction = deferred<void>();
      const admission = first.withPromotionAdmission(async () => {
        actionStarted.resolve();
        await finishAction.promise;
        return 'promoted';
      });
      await actionStarted.promise;
      const pause = second.updatePromotionControl(
        {
          promotionsEnabled: false,
          expectedRevision: 1,
          reason: 'incident response pause',
          updatedBy: 'write_authority',
        },
        new Date('2026-08-22T12:01:00.000Z'),
      );
      await expect(
        Promise.race([
          pause.then(() => 'paused'),
          new Promise<string>((resolve) => setTimeout(() => resolve('waiting'), 50)),
        ]),
      ).resolves.toBe('waiting');

      finishAction.resolve();
      await expect(admission).resolves.toEqual({ admitted: true, value: 'promoted' });
      await expect(pause).resolves.toMatchObject({ promotionsEnabled: false, revision: 2 });
    } finally {
      await Promise.all([first.close(), second.close()]);
    }

    const reopened = new PostgresUpgradeRepository(scopedUrl);
    try {
      await reopened.migrate();
      await expect(reopened.promotionControl()).resolves.toMatchObject({
        promotionsEnabled: false,
        revision: 2,
        reason: 'incident response pause',
      });
      await expect(reopened.withPromotionAdmission(async () => 'blocked')).resolves.toMatchObject({
        admitted: false,
        control: { revision: 2 },
      });
    } finally {
      await reopened.close();
    }
  });
});

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

const legacySchema = `
  CREATE TABLE mizuki_upgrades (
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
`;
