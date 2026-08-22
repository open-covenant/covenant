import { randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createRescueBounty } from './domain/index.js';
import { PostgresStore, StateConflictError } from './store.js';
import type { Quote } from './types.js';

const databaseUrl = process.env.MIZUKI_TEST_DATABASE_URL;

describe.skipIf(!databaseUrl)('PostgresStore integration', () => {
  let store: PostgresStore;

  beforeAll(async () => {
    store = await PostgresStore.connect(databaseUrl!);
  });

  afterAll(async () => {
    await store.close();
  });

  it('serializes idempotent job creation and state claims', async () => {
    const quote = await saveQuote(store);
    const payment = { payer: '1'.repeat(32), transaction: 'settlement', amountAtomic: '2000000' };
    const [left, right] = await Promise.all([
      store.createJob(quote, payment, 'postgres-idempotency'),
      store.createJob(quote, payment, 'postgres-idempotency'),
    ]);
    expect(new Set([left.job.id, right.job.id]).size).toBe(1);
    const job = left.job;
    await store.transitionJob(job.id, 'settlement_pending', 'paid');
    const claims = await Promise.allSettled([
      store.transitionJob(job.id, 'paid', 'admitted'),
      store.transitionJob(job.id, 'paid', 'admitted'),
    ]);
    expect(claims.filter((claim) => claim.status === 'fulfilled')).toHaveLength(1);
    expect(
      claims.some(
        (claim) => claim.status === 'rejected' && claim.reason instanceof StateConflictError,
      ),
    ).toBe(true);
  });

  it('serializes one payment proof across different request keys', async () => {
    const quote = await saveQuote(store);
    const payment = {
      payer: '4'.repeat(32),
      transaction: 'pending',
      amountAtomic: '2000000',
      signature: `proof-${randomUUID()}`,
    };
    const [left, right] = await Promise.all([
      store.createJob(quote, payment, `proof-left-${randomUUID()}`),
      store.createJob(quote, payment, `proof-right-${randomUUID()}`),
    ]);
    expect(new Set([left.job.id, right.job.id]).size).toBe(1);
    expect([left.created, right.created].filter(Boolean)).toHaveLength(1);
  });

  it('serializes one job per quote across different payments', async () => {
    const quote = await saveQuote(store);
    const [left, right] = await Promise.all([
      store.createJob(
        quote,
        {
          payer: '5'.repeat(32),
          transaction: 'pending',
          amountAtomic: '2000000',
          signature: `quote-proof-left-${randomUUID()}`,
        },
        `quote-left-${randomUUID()}`,
      ),
      store.createJob(
        quote,
        {
          payer: '6'.repeat(32),
          transaction: 'pending',
          amountAtomic: '2000000',
          signature: `quote-proof-right-${randomUUID()}`,
        },
        `quote-right-${randomUUID()}`,
      ),
    ]);
    expect(new Set([left.job.id, right.job.id]).size).toBe(1);
    expect([left.created, right.created].filter(Boolean)).toHaveLength(1);
    await expect(store.jobByQuote(quote.id)).resolves.toMatchObject({ id: left.job.id });
  });

  it('stores separate terminal bounty generations without duplicating one generation', async () => {
    const quote = await saveQuote(store);
    const { job } = await store.createJob(
      quote,
      { payer: '2'.repeat(32), transaction: 'settlement-2', amountAtomic: '2000000' },
      `generation-${randomUUID()}`,
    );
    const first = bounty(job.id, quote, 0);
    const replacement = bounty(job.id, quote, 1, first.id);
    expect((await store.createBounty(first)).created).toBe(true);
    expect((await store.createBounty(first)).created).toBe(false);
    expect((await store.createBounty(replacement)).created).toBe(true);
    expect(await store.bountyBySourceJob(job.id)).toMatchObject({
      id: replacement.id,
      generation: 1,
      predecessorBountyId: first.id,
    });
  });

  it('retries failed webhook work and rejects stale lease completion', async () => {
    const first = await store.beginWebhookDelivery('postgres-delivery');
    expect(first.state).toBe('started');
    if (first.state !== 'started') throw new Error('expected a webhook lease');
    await store.failWebhookDelivery('postgres-delivery', first.leaseId, 'temporary');
    const retry = await store.beginWebhookDelivery('postgres-delivery');
    expect(retry.state).toBe('started');
    if (retry.state !== 'started') throw new Error('expected a retry lease');
    await expect(store.completeWebhookDelivery('postgres-delivery', first.leaseId)).rejects.toThrow(
      'no longer active',
    );
    await store.completeWebhookDelivery('postgres-delivery', retry.leaseId);
    await expect(store.beginWebhookDelivery('postgres-delivery')).resolves.toEqual({
      state: 'completed',
    });
  });

  it('publishes a deterministic activity event exactly once', async () => {
    const eventId = randomUUID();
    const values = await Promise.all([
      store.appendActivity(
        'bounty.disputed',
        'bounty-postgres',
        { disputeId: 'dispute-postgres' },
        eventId,
      ),
      store.appendActivity(
        'bounty.disputed',
        'bounty-postgres',
        { disputeId: 'dispute-postgres' },
        eventId,
      ),
    ]);
    expect(values[0]).toEqual(values[1]);
  });

  it('recovers operator admission controls after reconnecting', async () => {
    const changed = await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'recovery persistence integration test',
      updatedBy: 'operator',
    });
    const reopened = await PostgresStore.connect(databaseUrl!);
    try {
      await expect(reopened.operatorControls()).resolves.toEqual(changed);
    } finally {
      await reopened.close();
      await store.updateOperatorControls({
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'integration test restored closed state',
        updatedBy: 'operator',
      });
    }
  });

  it('serializes and records schema migration reruns', async () => {
    const [left, right] = await Promise.all([
      PostgresStore.connect(databaseUrl!),
      PostgresStore.connect(databaseUrl!),
    ]);
    const pool = new Pool({ connectionString: databaseUrl });
    try {
      const result = await pool.query<{ version: number; name: string; checksum: string }>(
        `SELECT version, name, checksum FROM mizuki_schema_migrations
         WHERE component = 'core' ORDER BY version`,
      );
      expect(result.rows).toMatchObject([{ version: 1, name: 'commercial-core' }]);
      expect(result.rows[0]?.checksum).toMatch(/^[a-f0-9]{64}$/);
    } finally {
      await Promise.all([left.close(), right.close(), pool.end()]);
    }
  });
});

async function saveQuote(store: PostgresStore): Promise<Quote> {
  const id = randomUUID();
  return store.saveQuote({
    id,
    issueUrl: 'https://github.com/example/project/issues/1',
    owner: 'example',
    repo: 'project',
    issueNumber: 1,
    issueTitle: 'Fix docs',
    issueBody: '',
    baseSha: 'a'.repeat(40),
    defaultBranch: 'main',
    class: 'micro',
    priceAtomic: '2000000',
    maxFiles: 3,
    maxCostUsd: 0.8,
    validationCommands: [],
    expiresAt: '2099-01-01T00:00:00.000Z',
  });
}

function bounty(
  sourceJobId: string,
  quote: Quote,
  generation: number,
  predecessorBountyId?: string,
) {
  return createRescueBounty({
    id: randomUUID(),
    sourceJobId,
    failureReceiptId: `failure:${sourceJobId}`,
    repository: `${quote.owner}/${quote.repo}`,
    issueNumber: quote.issueNumber,
    issueUrl: quote.issueUrl,
    jobPriceCents: 200,
    generation,
    predecessorBountyId,
    at: new Date().toISOString(),
  });
}
