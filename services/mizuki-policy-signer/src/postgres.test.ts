import { randomBytes, randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { describe, expect, it } from 'vitest';
import type { RepositoryAdmission } from './domain.js';
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
           'mizuki_signer_payment_intents',
           'mizuki_signer_refund_commands',
           'mizuki_signer_refund_attempts',
           'mizuki_signer_bind_challenges',
           'mizuki_signer_repository_admissions'
         ) ORDER BY table_name`,
      );

      expect(migrations.rows).toMatchObject([
        { version: 1, name: 'policy-and-custody-core' },
        { version: 2, name: 'repository-admission-receipts' },
        { version: 3, name: 'delayed-liability-safety' },
        { version: 4, name: 'payment-intents-and-retryable-refunds' },
      ]);
      expect(migrations.rows.every((row) => /^[a-f0-9]{64}$/.test(row.checksum))).toBe(true);
      expect(tables.rows.map((row) => row.name)).toEqual([
        'mizuki_signer_bind_challenges',
        'mizuki_signer_operations',
        'mizuki_signer_payment_intents',
        'mizuki_signer_refund_attempts',
        'mizuki_signer_refund_commands',
        'mizuki_signer_refund_liabilities',
        'mizuki_signer_repository_admissions',
      ]);
    } finally {
      await Promise.all([left.close(), right.close(), pool.end()]);
    }
  });

  it('restores an idempotent repository admission from durable storage', async () => {
    const writer = new PostgresOperationStore(databaseUrl!);
    const restarted = new PostgresOperationStore(databaseUrl!);
    const admittedAt = new Date();
    const admission: RepositoryAdmission = {
      id: randomUUID(),
      idempotencyKey: `repository-admission-${randomUUID()}`,
      requestHash: randomBytes(32).toString('hex'),
      quoteId: randomUUID(),
      repository: 'owner/repository',
      issueNumber: 17,
      baseRef: 'main',
      baseSha: 'b'.repeat(40),
      reservationKeyHash: randomBytes(32).toString('hex'),
      paymentAuthorizationHash: randomBytes(32).toString('hex'),
      settlementMessageHash: randomBytes(32).toString('hex'),
      settlementClientSignature: '3'.repeat(64),
      settlementFeePayer: '4'.repeat(32),
      settlementPayer: '5'.repeat(32),
      settlementMemo: `mizuki:payment:v1:${randomUUID()}`,
      settlementRawAmount: '2000000',
      paymentWindowStartUnixSeconds: 1_774_182_370,
      paymentWindowEndUnixSeconds: 1_774_182_730,
      verifierAppId: '12345',
      installationId: 777,
      repositorySelection: 'selected',
      permissions: {
        contents: 'read',
        issues: 'read',
        metadata: 'read',
        pull_requests: 'read',
      },
      tokenRepositories: 1,
      tokenExpiresAt: new Date(admittedAt.getTime() + 60 * 60_000),
      admittedAt,
      evidenceHash: randomBytes(32).toString('hex'),
    };
    try {
      await writer.migrate();
      const first = await writer.registerRepositoryAdmission(admission);
      const replay = await writer.registerRepositoryAdmission({
        ...admission,
        id: randomUUID(),
      });
      await restarted.migrate();
      const restored = await restarted.getRepositoryAdmission(admission.id);
      const restoredByKey = await restarted.getRepositoryAdmissionByIdempotencyKey(
        admission.idempotencyKey,
      );

      expect(replay.id).toBe(first.id);
      expect(restored).toEqual(first);
      expect(restoredByKey).toEqual(first);
    } finally {
      await Promise.all([writer.close(), restarted.close()]);
    }
  });
});
