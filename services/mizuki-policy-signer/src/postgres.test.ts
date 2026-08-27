import { randomBytes, randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { describe, expect, it } from 'vitest';
import type {
  OperationRecord,
  PaymentIntent,
  RefundLiability,
  RepositoryAdmission,
  ReserveOperation,
} from './domain.js';
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
        { version: 5, name: 'bounty-source-job-handoffs' },
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
    try {
      await store.migrate();
      const before = BigInt(await store.pendingBountyReserveLamports());
      const { intent, liability } = await activatedSource(store, now);

      expect(await store.getPaymentIntentByJob(intent.jobId)).toMatchObject({ id: intent.id });
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

  it('permits only one concurrent escrow handoff for a source job', async () => {
    const left = new PostgresOperationStore(databaseUrl!);
    const right = new PostgresOperationStore(databaseUrl!);
    const now = new Date();
    try {
      await left.migrate();
      const source = await refundedSource(left, now);
      const leftReservation = escrowReservation(source, `bounty-${randomUUID()}`);
      const rightReservation = escrowReservation(source, `bounty-${randomUUID()}`);

      const results = await Promise.allSettled([
        left.reserve(leftReservation, 1_000_000_000, now),
        right.reserve(rightReservation, 1_000_000_000, now),
      ]);
      const fulfilled = results.filter(
        (result): result is PromiseFulfilledResult<OperationRecord> =>
          result.status === 'fulfilled',
      );
      const rejected = results.filter(
        (result): result is PromiseRejectedResult => result.status === 'rejected',
      );

      expect(fulfilled).toHaveLength(1);
      expect(rejected).toHaveLength(1);
      expect(rejected[0]?.reason).toMatchObject({ code: 'bounty_handoff_active' });
      const persisted = await Promise.all([
        left.getByIdempotencyKey(leftReservation.idempotencyKey),
        left.getByIdempotencyKey(rightReservation.idempotencyKey),
      ]);
      expect(persisted.filter(Boolean)).toHaveLength(1);
    } finally {
      await Promise.all([left.close(), right.close()]);
    }
  });

  it('rejects an escrow handoff above the source payment bounty reserve', async () => {
    const store = new PostgresOperationStore(databaseUrl!);
    const now = new Date();
    try {
      await store.migrate();
      const source = await refundedSource(store, now);
      const reservation = escrowReservation(source, `bounty-${randomUUID()}`);
      reservation.details.amountLamports = (
        BigInt(source.intent.bountyReserveLamports) + 1n
      ).toString();

      await expect(store.reserve(reservation, 1_000_000_000, now)).rejects.toMatchObject({
        code: 'bounty_reserve_price_drift',
        retryable: true,
      });
      await expect(store.getByIdempotencyKey(reservation.idempotencyKey)).resolves.toBeNull();
    } finally {
      await store.close();
    }
  });

  it('restores bounty capacity after escrow refund and permits one replacement', async () => {
    const left = new PostgresOperationStore(databaseUrl!);
    const right = new PostgresOperationStore(databaseUrl!);
    const now = new Date();
    try {
      await left.migrate();
      const before = BigInt(await left.pendingBountyReserveLamports());
      const source = await refundedSource(left, now);
      const initial = await left.reserve(
        escrowReservation(source, `bounty-${randomUUID()}`),
        1_000_000_000,
        now,
      );
      await finalize(left, initial, now);
      expect(BigInt(await left.pendingBountyReserveLamports())).toBe(before);

      const refund = await left.reserve(escrowRefund(initial.id), 1_000_000_000, now);
      await finalize(left, refund, now);
      expect(BigInt(await left.pendingBountyReserveLamports())).toBe(
        before + BigInt(source.intent.bountyReserveLamports),
      );

      const leftReplacement = escrowReservation(source, `bounty-${randomUUID()}`);
      const rightReplacement = escrowReservation(source, `bounty-${randomUUID()}`);
      const results = await Promise.allSettled([
        left.reserve(leftReplacement, 1_000_000_000, now),
        right.reserve(rightReplacement, 1_000_000_000, now),
      ]);
      const fulfilled = results.filter(
        (result): result is PromiseFulfilledResult<OperationRecord> =>
          result.status === 'fulfilled',
      );
      const rejected = results.filter(
        (result): result is PromiseRejectedResult => result.status === 'rejected',
      );

      expect(fulfilled).toHaveLength(1);
      expect(rejected).toHaveLength(1);
      expect(rejected[0]?.reason).toMatchObject({ code: 'bounty_handoff_active' });
      await finalize(left, fulfilled[0]!.value, now);
      expect(BigInt(await left.pendingBountyReserveLamports())).toBe(before);
    } finally {
      await Promise.all([left.close(), right.close()]);
    }
  });
});

const paymentIntentLimits = {
  refundCapacityRaw: '1000000000000',
  bountyCapacityLamports: '1000000000000',
  refundSignerLamports: '1000000000000',
  refundCostLamports: '1',
  refundDailyLimitUsdCents: 1_000_000_000,
  escrowDailyLimitUsdCents: 1_000_000_000,
};

async function refundedSource(store: PostgresOperationStore, now: Date) {
  const source = await activatedSource(store, now);
  const refund = await store.reserve(refundOperation(source.liability), 1_000_000_000, now);
  await finalize(store, refund, now);
  return source;
}

async function activatedSource(store: PostgresOperationStore, now: Date) {
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
  await store.registerRepositoryAdmission(admission);
  await store.reservePaymentIntent(intent, paymentIntentLimits, now);
  await store.activatePaymentIntent(intent.id, liability, `activate-${randomUUID()}`, now);
  return { admission, intent, liability };
}

function refundOperation(liability: RefundLiability): ReserveOperation {
  return {
    id: randomUUID(),
    idempotencyKey: `refund-${randomUUID()}`,
    resourceKey: `refund:${liability.settlementSignature}`,
    requestHash: randomBytes(32).toString('hex'),
    kind: 'refund',
    amountUsdCents: liability.amountUsdCents,
    spendBucket: 'none',
    asset: liability.mint,
    recipient: liability.payer,
    details: { jobId: liability.jobId, settlementSignature: liability.settlementSignature },
  };
}

function escrowReservation(
  source: Awaited<ReturnType<typeof refundedSource>>,
  bountyId: string,
): ReserveOperation {
  return {
    id: randomUUID(),
    idempotencyKey: `escrow-${randomUUID()}`,
    resourceKey: `escrow:${bountyId}`,
    requestHash: randomBytes(32).toString('hex'),
    kind: 'escrow_reserve',
    amountUsdCents: source.intent.bountyAmountUsdCents,
    spendBucket: 'escrow',
    asset: 'SOL',
    recipient: 'escrow-vault',
    details: {
      bountyId,
      sourceJobId: source.intent.jobId,
      amountLamports: source.intent.bountyReserveLamports,
      repository: source.intent.repository,
      issueNumber: source.intent.issueNumber,
      baseRef: source.intent.baseRef,
      baseSha: source.intent.baseSha,
    },
  };
}

function escrowRefund(escrowOperationId: string): ReserveOperation {
  return {
    id: randomUUID(),
    idempotencyKey: `escrow-refund-${randomUUID()}`,
    resourceKey: `escrow_resolution:${escrowOperationId}`,
    requestHash: randomBytes(32).toString('hex'),
    kind: 'escrow_refund',
    amountUsdCents: 0,
    spendBucket: 'none',
    asset: 'SOL',
    recipient: 'escrow-authority',
    details: { escrowOperationId },
  };
}

async function finalize(
  store: PostgresOperationStore,
  operation: OperationRecord,
  now: Date,
): Promise<OperationRecord> {
  const owner = randomUUID();
  const leased = await store.acquireLease(operation.id, owner, now, 30_000);
  if (!leased) throw new Error(`could not lease operation ${operation.id}`);
  return store.update(operation.id, owner, leased.version, {
    status: 'finalized',
    transactionSignature: randomBytes(64).toString('base64url'),
  });
}

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
    settlementClientSignature: randomBase58(64),
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

function randomBase58(length: number): string {
  const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  return [...randomBytes(length)].map((value) => alphabet[value % alphabet.length]).join('');
}
