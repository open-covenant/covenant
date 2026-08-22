import { Pool } from 'pg';
import { describe, expect, it } from 'vitest';
import { PostgresOperationStore } from './postgres.js';

const databaseUrl = process.env.MIZUKI_SIGNER_TEST_DATABASE_URL;

describe.skipIf(!databaseUrl)('PostgresOperationStore migrations', () => {
  it('serializes concurrent migration reruns and records the applied version', async () => {
    const left = new PostgresOperationStore(databaseUrl!);
    const right = new PostgresOperationStore(databaseUrl!);
    const pool = new Pool({ connectionString: databaseUrl });
    try {
      await Promise.all([left.migrate(), right.migrate()]);
      const migrations = await pool.query<{ version: number; name: string; checksum: string }>(
        `SELECT version, name, checksum FROM mizuki_schema_migrations
         WHERE component = 'policy-signer' ORDER BY version`,
      );
      const tables = await pool.query<{ name: string }>(
        `SELECT table_name AS name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name IN (
           'mizuki_signer_operations',
           'mizuki_signer_refund_liabilities',
           'mizuki_signer_bind_challenges'
         ) ORDER BY table_name`,
      );

      expect(migrations.rows).toMatchObject([{ version: 1, name: 'policy-and-custody-core' }]);
      expect(migrations.rows[0]?.checksum).toMatch(/^[a-f0-9]{64}$/);
      expect(tables.rows.map((row) => row.name)).toEqual([
        'mizuki_signer_bind_challenges',
        'mizuki_signer_operations',
        'mizuki_signer_refund_liabilities',
      ]);
    } finally {
      await Promise.all([left.close(), right.close(), pool.end()]);
    }
  });
});
