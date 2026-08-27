import { randomBytes, randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { describe, expect, it } from 'vitest';
import type { PaymentIntent, RefundLiability, RepositoryAdmission } from './domain.js';
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

  it('releases a payment intent bounty reserve after its liability is discharged', async () => {
    const store = new PostgresOperationStore(databaseUrl!);
    const now = new Date();
    const admission = repositoryAdmission(now);
    const jobId = `job-${randomUUID()}`;
    const intent: PaymentIntent = {
      id: randomUUID(),
      idempotencyKey: `payment-intent-${randomUUID()}`,
      requestHash: randomBytes(32).toString('hex'),
      jobId,
      quoteId: admission.quoteId,
      repositoryAdmissionId: admission.id,
      repositoryAdmissionEvidenceHash: admission.evidenceHash,
      repository: admission.repository,
      issueNumber: admission.issueNumber,
      baseRef: admission.baseRef,
      baseSha: admission.baseSha,
      repositoryAuthorizedAt: admission.admittedAt,
      authorizationEvidenceHash: randomBytes(32).toString('hex'),
      payer: admission.settlementPayer!,
      payee: '6'.repeat(32),
      mint: '7'.repeat(32),
      rawAmount: '2000000',
      amountUsdCents: 200,
      bountyAmountUsdCents: 1_000,
      bountyReserveLamports: '12345',
      memo: admission.settlementMemo!,
      signedMessageHash: admission.settlementMessageHash,
      payerSignature: admission.settlementClientSignature,
      paymentWindowStartUnixSeconds: admission.paymentWindowStartUnixSeconds,
      paymentWindowEndUnixSeconds: admission.paymentWindowEndUnixSeconds,
      status: 'reserved',
      settlementSignature: null,
      liabilityId: null,
      activationIdempotencyKey: null,
      createdAt: now,
      activatedAt: null,
      expiredAt: null,
    };
    const liability: RefundLiability = {
      id: randomUUID(),
      idempotencyKey: `liability-${randomUUID()}`,
      requestHash: randomBytes(32).toString('hex'),
      jobId,
      repositoryAdmissionId: admission.id,
      settlementSignature: randomBytes(64).toString('base64url'),
      repository: admission.repository,
      issueNumber: admission.issueNumber,
      baseRef: admission.baseRef,
      baseSha: admission.baseSha,
      repositoryAuthorizedAt: admission.admittedAt,
      authorizationEvidenceHash: intent.authorizationEvidenceHash,
      reviewedHeadSha: null,
      reviewedBaseSha: null,
      reviewedBaseRef: null,
      reviewedDiffHash: null,
      deliveryBoundAt: null,
      deliveryBindingIdempotencyKey: null,
      deliveryBindingRequestHash: null,
      deliveryBindingHash: null,
      payer: intent.payer,
      treasury: intent.payee,
      mint: intent.mint,
      rawAmount: intent.rawAmount,
      decimals: 6,
      amountUsdCents: intent.amountUsdCents,
      settlementSlot: 42,
      settlementBlockTimeUnixSeconds: Math.floor(now.getTime() / 1_000),
      createdAt: now,
      dischargedAt: null,
      dischargeEvidenceHash: null,
      dischargeEvidence: null,
      dischargeIdempotencyKey: null,
      dischargeRequestHash: null,
    };
    try {
      await store.migrate();
      const before = BigInt(await store.pendingBountyReserveLamports());
      await store.registerRepositoryAdmission(admission);
      await store.reservePaymentIntent(
        intent,
        {
          refundCapacityRaw: '1000000000000',
          bountyCapacityLamports: '1000000000000',
          refundSignerLamports: '1000000000000',
          refundCostLamports: '1',
          refundDailyLimitUsdCents: 1_000_000_000,
          escrowDailyLimitUsdCents: 1_000_000_000,
        },
        now,
      );
      await store.activatePaymentIntent(intent.id, liability, `activate-${randomUUID()}`, now);

      expect(await store.getPaymentIntentByJob(jobId)).toMatchObject({ id: intent.id });
      expect(BigInt(await store.pendingBountyReserveLamports())).toBe(before + 12_345n);

      await store.dischargeRefundLiability(
        liability.id,
        `discharge-${randomUUID()}`,
        randomBytes(32).toString('hex'),
        randomBytes(32).toString('hex'),
        { outcome: 'merged' },
        now,
      );
      expect(BigInt(await store.pendingBountyReserveLamports())).toBe(before);
    } finally {
      await store.close();
    }
  });
});

function repositoryAdmission(admittedAt: Date): RepositoryAdmission {
  const quoteId = randomUUID();
  return {
    id: randomUUID(),
    idempotencyKey: `repository-admission-${randomUUID()}`,
    requestHash: randomBytes(32).toString('hex'),
    quoteId,
    repository: 'owner/repository',
    issueNumber: 17,
    baseRef: 'main',
    baseSha: 'b'.repeat(40),
    reservationKeyHash: randomBytes(32).toString('hex'),
    paymentAuthorizationHash: randomBytes(32).toString('hex'),
    settlementMessageHash: randomBytes(32).toString('hex'),
    settlementClientSignature: '8'.repeat(64),
    settlementFeePayer: '4'.repeat(32),
    settlementPayer: '5'.repeat(32),
    settlementMemo: `mizuki:payment:v1:${quoteId}`,
    settlementRawAmount: '2000000',
    paymentWindowStartUnixSeconds: Math.floor(admittedAt.getTime() / 1_000),
    paymentWindowEndUnixSeconds: Math.floor(admittedAt.getTime() / 1_000) + 360,
    verifierAppId: '12345',
    installationId: 777,
    repositorySelection: 'selected',
    permissions: {
      checks: 'read',
      contents: 'read',
      issues: 'read',
      metadata: 'read',
      pull_requests: 'read',
      statuses: 'read',
    },
    tokenRepositories: 1,
    tokenExpiresAt: new Date(admittedAt.getTime() + 60 * 60_000),
    admittedAt,
    evidenceHash: randomBytes(32).toString('hex'),
  };
}
