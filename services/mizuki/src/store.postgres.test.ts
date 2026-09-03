import { createHash, randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { API_TOKEN_TERMINAL_HISTORY_LIMIT, createApiToken } from './api-tokens.js';
import { ensurePaymentCapacity } from './app.js';
import {
  type BountyClaim,
  calculateRescueBountyPriceCents,
  createContributorEscrow,
  createRescueBounty,
  transitionContributorEscrow,
} from './domain/index.js';
import {
  COMMERCIAL_CORE_SCHEMA_V1,
  GithubOAuthCapacityError,
  MAX_PENDING_GITHUB_OAUTH_FLOWS,
  PostgresStore,
  SOCIAL_POSTS_SCHEMA_V1,
  StateConflictError,
  WORKBENCH_API_TOKENS_SCHEMA_V1,
} from './store.js';
import { RefundCapacityError } from './policy-client.js';
import type { Quote, SocialPostReceipt } from './types.js';

const databaseUrl = process.env.MIZUKI_TEST_DATABASE_URL;
const DEPLOYED_CORE_V1_CHECKSUM =
  '1e1c7b752aead2d673a8d82fba69113344ada76444a1263e6bc80bffb0d80429';
const WORKBENCH_API_TOKENS_V1_CHECKSUM =
  '4787de73a64016308c8823bcbd209e0638a1d8fa57b3c3a2f2517a86120c412b';
const SOCIAL_POSTS_V1_CHECKSUM = '8785618d0a00ad135060f64d37fd786215042104ac9f73f081396a9ca8babd7d';

describe('PostgresStore schema', () => {
  it('keeps the API token migration immutable', () => {
    expect(createHash('sha256').update(WORKBENCH_API_TOKENS_SCHEMA_V1).digest('hex')).toBe(
      WORKBENCH_API_TOKENS_V1_CHECKSUM,
    );
  });

  it('keeps the social receipt migration immutable', () => {
    expect(createHash('sha256').update(SOCIAL_POSTS_SCHEMA_V1).digest('hex')).toBe(
      SOCIAL_POSTS_V1_CHECKSUM,
    );
  });
});

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

  it('persists only hashed API token credentials and revokes them atomically', async () => {
    const githubId = `token-${randomUUID()}`;
    await store.upsertContributor(githubId, 'token-maintainer');
    const credential = createApiToken({
      githubId,
      name: 'Postgres MCP',
      scopes: ['repositories:read', 'jobs:read'],
      expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
    });

    const stored = await store.createApiToken(credential.record);
    expect(JSON.stringify(stored)).not.toContain(credential.token);
    await expect(store.apiTokenByPrefix(stored.prefix)).resolves.toEqual(stored);
    const usedAt = new Date(Date.now() + 1_000).toISOString();
    await expect(store.markApiTokenUsed(stored.id, usedAt)).resolves.toBe(true);
    await expect(
      store.markApiTokenUsed(stored.id, new Date(Date.parse(usedAt) - 500).toISOString()),
    ).resolves.toBe(true);
    await expect(store.apiTokenByPrefix(stored.prefix)).resolves.toMatchObject({
      lastUsedAt: usedAt,
    });

    for (let index = 1; index <= 105; index += 1) {
      const terminal = createApiToken({
        githubId,
        name: `Terminal Postgres MCP ${index}`,
        scopes: ['jobs:read'],
        expiresAt: new Date(Date.now() + 24 * 60 * 60_000).toISOString(),
      });
      await store.createApiToken(terminal.record);
      await store.revokeApiToken(terminal.record.id, githubId, new Date().toISOString());
    }
    const listed = await store.apiTokensForAccount(githubId);
    expect(listed).toHaveLength(API_TOKEN_TERMINAL_HISTORY_LIMIT + 1);
    expect(listed[0]?.id).toBe(stored.id);

    const revoked = await store.revokeApiToken(stored.id, githubId, new Date().toISOString());
    expect(revoked?.revokedAt).toBeTruthy();
    await expect(store.markApiTokenUsed(stored.id, new Date().toISOString())).resolves.toBe(false);
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

  it('keeps expired unpaid reservations terminal across account recovery queries', async () => {
    const githubId = randomUUID();
    const quote = await saveQuote(store);
    await store.upsertContributor(githubId, 'payment-expiry-maintainer');
    await store.linkQuoteToAccount(quote.id, githubId);
    const attempt = await store.createPaymentAttempt({
      githubId,
      quoteId: quote.id,
      wallet: '1'.repeat(32),
      appBuild: 'release-test',
    });
    const { job } = await store.createJob(
      quote,
      {
        payer: '1'.repeat(32),
        transaction: 'pending',
        amountAtomic: quote.priceAtomic,
        signature: `signed-${randomUUID()}`,
      },
      attempt.idempotencyKey,
      undefined,
      attempt.id,
    );
    const paymentWindowEndUnixSeconds = 1_800_000_000;
    await store.patchJob(job.id, {
      paymentIntentId: '55555555-5555-4555-8555-555555555555',
      paymentWindowEndUnixSeconds,
    });
    await store.bindPaymentAttemptJob(
      attempt.id,
      githubId,
      job.id,
      undefined,
      paymentWindowEndUnixSeconds,
    );
    await expect(
      store.bindPaymentAttemptJob(
        attempt.id,
        githubId,
        job.id,
        undefined,
        paymentWindowEndUnixSeconds + 1,
      ),
    ).rejects.toThrow('payment authorization deadline does not match the job');
    const expired = await store.expirePaymentReservation(job.id, attempt.id);

    await expect(store.jobsForAccount(githubId, 100)).resolves.toEqual({
      jobs: [expired],
      limit: 100,
      truncated: false,
      obligationCount: 0,
    });
    await expect(
      store.paymentStatusForAccount(quote.id, githubId, attempt.idempotencyKey),
    ).resolves.toMatchObject({ kind: 'unpaid' });
    await expect(store.bindPaymentAttemptJob(attempt.id, githubId, job.id)).rejects.toThrow(
      'payment attempt has expired unpaid',
    );
    await expect(store.paymentAttempt(attempt.id, githubId)).resolves.toMatchObject({
      paymentWindowEndUnixSeconds,
    });
  });

  it('uses one lock order when recovery binding races payment expiry', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    const githubId = randomUUID();
    const quote = await saveQuote(store);
    await store.upsertContributor(githubId, 'payment-lock-maintainer');
    await store.linkQuoteToAccount(quote.id, githubId);
    const attempt = await store.createPaymentAttempt({
      githubId,
      quoteId: quote.id,
      wallet: '1'.repeat(32),
      appBuild: 'release-test',
    });
    await store.updatePaymentAttemptStage(attempt.id, githubId, 'submitting', 'server');
    const { job } = await store.createJob(
      quote,
      {
        payer: '1'.repeat(32),
        transaction: 'pending',
        amountAtomic: quote.priceAtomic,
        signature: `signed-${randomUUID()}`,
      },
      attempt.idempotencyKey,
      undefined,
      attempt.id,
    );

    try {
      const [binding, expiry] = await Promise.allSettled([
        store.bindPaymentAttemptJob(attempt.id, githubId, job.id),
        peer.expirePaymentReservation(job.id, attempt.id),
      ]);
      expect(expiry.status).toBe('fulfilled');
      if (binding.status === 'rejected') {
        expect(binding.reason).toBeInstanceOf(StateConflictError);
      }
      await expect(store.job(job.id)).resolves.toMatchObject({ state: 'payment_expired' });
      await expect(store.paymentAttempt(attempt.id, githubId)).resolves.toMatchObject({
        stage: 'expired_unpaid',
        retrySafe: true,
      });
    } finally {
      await peer.close();
    }
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

  it('persists contributor escrow state transitions', async () => {
    const quote = await saveQuote(store);
    const { job } = await store.createJob(
      quote,
      { payer: '7'.repeat(32), transaction: 'settlement-7', amountAtomic: '2000000' },
      `escrow-${randomUUID()}`,
    );
    const createdBounty = bounty(job.id, quote, 0);
    await store.createBounty(createdBounty);
    const requested = createContributorEscrow({
      id: randomUUID(),
      bountyId: createdBounty.id,
      repository: `${quote.owner}/${quote.repo}`,
      issueNumber: quote.issueNumber,
      issueTitle: quote.issueTitle,
      issueBody: quote.issueBody,
      baseRef: quote.defaultBranch,
      baseSha: quote.baseSha,
      reviewPolicy: { version: 1, model: 'independent-reviewer', maxFiles: 3 },
      amountCents: 1_000,
      acceptanceHash: 'a'.repeat(64),
      expiresAt: '2099-01-02T00:00:00.000Z',
      at: '2099-01-01T00:00:00.000Z',
    });
    await store.saveEscrow(requested);
    const funding = transitionContributorEscrow(requested, 'funding', {
      expectedRevision: requested.revision,
      at: '2099-01-01T00:01:00.000Z',
    });

    await expect(store.saveEscrow(funding)).resolves.toEqual(funding);
    await expect(store.escrow(requested.id)).resolves.toEqual(funding);
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

  it('persists one immutable receipt per social source and post', async () => {
    const receipt = socialReceipt();
    await expect(store.saveSocialPost(receipt)).resolves.toEqual(receipt);
    await expect(store.saveSocialPost(receipt)).resolves.toEqual(receipt);
    await expect(store.socialPosts()).resolves.toContainEqual(receipt);
    await expect(
      store.saveSocialPost({ ...receipt, id: randomUUID(), text: 'different text' }),
    ).rejects.toBeInstanceOf(StateConflictError);

    const pool = new Pool({ connectionString: databaseUrl });
    try {
      await expect(
        pool.query('UPDATE mizuki_social_posts SET post_id = $1 WHERE id = $2', [
          randomUUID(),
          receipt.id,
        ]),
      ).rejects.toThrow('social post receipts are append-only');
      await expect(
        pool.query('DELETE FROM mizuki_social_posts WHERE id = $1', [receipt.id]),
      ).rejects.toThrow('social post receipts are append-only');
      await expect(pool.query('TRUNCATE mizuki_social_posts')).rejects.toThrow(
        'social post receipts are append-only',
      );
    } finally {
      await pool.end();
    }
  });

  it('recovers operator admission controls after reconnecting', async () => {
    const initial = await store.operatorControls();
    const changed = await store.updateOperatorControls({
      expectedRevision: initial.revision,
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
        expectedRevision: changed.revision,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'integration test restored closed state',
        updatedBy: 'operator',
      });
    }
  });

  it('keeps concurrent admission closure fail-safe and the audit append-only', async () => {
    const initial = await store.operatorControls();
    expect(initial).toMatchObject({ intakeEnabled: false, claimsEnabled: false });
    const results = await Promise.allSettled([
      store.updateOperatorControls({
        expectedRevision: initial.revision,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'concurrent integration test opening request',
        updatedBy: 'operator',
      }),
      store.updateOperatorControls({
        expectedRevision: initial.revision,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'concurrent integration test emergency close',
        updatedBy: 'operator',
      }),
    ]);
    expect(results.some((result) => result.status === 'fulfilled')).toBe(true);

    const closed = await store.operatorControls();
    expect(closed).toMatchObject({ intakeEnabled: false, claimsEnabled: false });
    const beforeStale = await store.operatorControlsAudit();
    await expect(
      store.updateOperatorControls({
        expectedRevision: initial.revision,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'stale integration test reopening request',
        updatedBy: 'operator',
      }),
    ).rejects.toThrow('expected operator admission revision');
    await expect(store.operatorControlsAudit()).resolves.toEqual(beforeStale);
    expect(beforeStale.at(-1)).toMatchObject(closed);

    const pool = new Pool({ connectionString: databaseUrl });
    try {
      await pool.query(
        'UPDATE mizuki_operator_controls SET intake_enabled = true WHERE singleton = true',
      );
      try {
        await expect(store.operatorControls()).rejects.toThrow(
          'operator admission controls are unavailable or unaudited',
        );
        const auditBeforeRejectedMutation = await store.operatorControlsAudit();
        await expect(
          store.updateOperatorControls({
            expectedRevision: closed.revision,
            intakeEnabled: false,
            claimsEnabled: false,
            reason: 'tampered current state cannot enter the audit',
            updatedBy: 'operator',
          }),
        ).rejects.toThrow('operator admission controls are unavailable or unaudited');
        await expect(store.operatorControlsAudit()).resolves.toEqual(auditBeforeRejectedMutation);
      } finally {
        await pool.query(
          'UPDATE mizuki_operator_controls SET intake_enabled = $1 WHERE singleton = true',
          [closed.intakeEnabled],
        );
      }
      await expect(store.operatorControls()).resolves.toEqual(closed);
      await expect(
        pool.query('UPDATE mizuki_operator_control_audit SET reason = $1 WHERE revision = $2', [
          'tampered audit entry',
          closed.revision,
        ]),
      ).rejects.toThrow('operator control audit is append-only');
      await expect(
        pool.query('DELETE FROM mizuki_operator_control_audit WHERE revision = $1', [
          closed.revision,
        ]),
      ).rejects.toThrow('operator control audit is append-only');
      await expect(pool.query('TRUNCATE mizuki_operator_control_audit')).rejects.toThrow(
        'operator control audit is append-only',
      );
      await expect(
        pool.query(
          `INSERT INTO mizuki_operator_control_audit (
             revision, expected_revision, intake_enabled, claims_enabled, reason,
             updated_by, updated_at
           ) VALUES (100, 101, false, false, 'invalid future revision', 'operator', now())`,
        ),
      ).rejects.toThrow('violates check constraint');
    } finally {
      await pool.end();
    }
  });

  it('serializes commercial admission across independent runtime pools', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    let releaseFirst!: () => void;
    let markFirstEntered!: () => void;
    const holdFirst = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const firstEntered = new Promise<void>((resolve) => {
      markFirstEntered = resolve;
    });
    const order: string[] = [];
    const first = store.withAdmissionLock(async () => {
      order.push('first');
      markFirstEntered();
      await holdFirst;
    });
    await firstEntered;
    const second = peer.withAdmissionLock(async () => {
      order.push('second');
    });

    try {
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(order).toEqual(['first']);
      releaseFirst();
      await Promise.all([first, second]);
      expect(order).toEqual(['first', 'second']);

      await expect(
        store.withAdmissionLock(async () => {
          throw new Error('simulated admission failure');
        }),
      ).rejects.toThrow('simulated admission failure');
      await expect(peer.withAdmissionLock(async () => 'released')).resolves.toBe('released');
    } finally {
      releaseFirst();
      await Promise.allSettled([first, second]);
      await peer.close();
    }
  });

  it('prevents a replacement attempt after another replica accepts the signed attempt', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    const githubId = randomUUID();
    const signedQuote = await saveQuote(store);
    const replacementQuote = await saveQuote(peer);
    await store.upsertContributor(githubId, 'signed-race-maintainer');
    await Promise.all([
      store.linkQuoteToAccount(signedQuote.id, githubId),
      peer.linkQuoteToAccount(replacementQuote.id, githubId),
    ]);
    const attempt = await store.createPaymentAttempt({
      githubId,
      quoteId: signedQuote.id,
      wallet: '1'.repeat(32),
      appBuild: 'release-test',
    });
    let releaseSigned!: () => void;
    let markSigned!: () => void;
    const signedHeld = new Promise<void>((resolve) => {
      releaseSigned = resolve;
    });
    const signedAccepted = new Promise<void>((resolve) => {
      markSigned = resolve;
    });
    const signed = store.withAdmissionLock(async () => {
      await store.updatePaymentAttemptStage(attempt.id, githubId, 'submitting', 'server');
      markSigned();
      await signedHeld;
    });
    await signedAccepted;
    const replacement = peer.withAdmissionLock(() =>
      peer.createPaymentAttempt({
        githubId,
        quoteId: replacementQuote.id,
        wallet: '1'.repeat(32),
        appBuild: 'release-test',
      }),
    );

    try {
      await new Promise((resolve) => setTimeout(resolve, 50));
      await expect(peer.activePaymentAttempt(githubId)).resolves.toMatchObject({ id: attempt.id });
      releaseSigned();
      await signed;
      await expect(replacement).rejects.toThrow('resolve the active payment attempt');
      await expect(store.paymentAttempt(attempt.id, githubId)).resolves.toMatchObject({
        stage: 'submitting',
        retrySafe: false,
      });
      await expect(store.quoteForAccount(replacementQuote.id, githubId)).resolves.toBeDefined();
      await expect(store.activePaymentAttempt(githubId)).resolves.toMatchObject({ id: attempt.id });
    } finally {
      releaseSigned();
      await Promise.allSettled([signed, replacement]);
      await peer.close();
    }
  });

  it('rejects a stale signed request after another replica atomically replaces it', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    const githubId = randomUUID();
    const staleQuote = await saveQuote(store);
    const replacementQuote = await saveQuote(peer);
    await store.upsertContributor(githubId, 'replacement-race-maintainer');
    await Promise.all([
      store.linkQuoteToAccount(staleQuote.id, githubId),
      peer.linkQuoteToAccount(replacementQuote.id, githubId),
    ]);
    const stale = await store.createPaymentAttempt({
      githubId,
      quoteId: staleQuote.id,
      wallet: '1'.repeat(32),
      appBuild: 'release-test',
    });
    let releaseReplacement!: () => void;
    let markReplacement!: () => void;
    const replacementHeld = new Promise<void>((resolve) => {
      releaseReplacement = resolve;
    });
    const replacementCreated = new Promise<void>((resolve) => {
      markReplacement = resolve;
    });
    let replacementId: string | undefined;
    const replacement = peer.withAdmissionLock(async () => {
      const created = await peer.createPaymentAttempt({
        githubId,
        quoteId: replacementQuote.id,
        wallet: '1'.repeat(32),
        appBuild: 'release-test',
      });
      replacementId = created.id;
      markReplacement();
      await replacementHeld;
      return created;
    });
    await replacementCreated;
    const signed = store.withAdmissionLock(() =>
      store.updatePaymentAttemptStage(stale.id, githubId, 'submitting', 'server'),
    );

    try {
      await new Promise((resolve) => setTimeout(resolve, 50));
      releaseReplacement();
      await expect(replacement).resolves.toMatchObject({ id: replacementId });
      await expect(signed).rejects.toThrow('payment attempt is expired_unpaid');
      await expect(store.paymentAttempt(stale.id, githubId)).resolves.toMatchObject({
        stage: 'expired_unpaid',
        retrySafe: true,
      });
      await expect(store.activePaymentAttempt(githubId)).resolves.toMatchObject({
        id: replacementId,
        stage: 'created',
      });
    } finally {
      releaseReplacement();
      await Promise.allSettled([replacement, signed]);
      await peer.close();
    }
  });

  it('authorizes exactly one prompt nonce across independent runtime pools', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    const githubId = randomUUID();
    const quote = await saveQuote(store);
    await store.upsertContributor(githubId, 'prompt-race-maintainer');
    await store.linkQuoteToAccount(quote.id, githubId);
    const attempt = await store.createPaymentAttempt({
      githubId,
      quoteId: quote.id,
      wallet: '1'.repeat(32),
      appBuild: 'release-test',
    });
    const firstNonce = randomUUID();
    const secondNonce = randomUUID();

    try {
      const results = await Promise.allSettled([
        store.authorizePaymentPrompt(attempt.id, githubId, firstNonce),
        peer.authorizePaymentPrompt(attempt.id, githubId, secondNonce),
      ]);
      expect(results.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
      expect(
        results.some(
          (result) =>
            result.status === 'rejected' &&
            result.reason instanceof StateConflictError &&
            result.reason.message === 'wallet prompt is already authorized in another client',
        ),
      ).toBe(true);
      const winner = results.find((result) => result.status === 'fulfilled');
      if (!winner || winner.status !== 'fulfilled') throw new Error('prompt race had no winner');
      await expect(store.paymentAttempt(attempt.id, githubId)).resolves.toMatchObject({
        stage: 'wallet_opened',
        retrySafe: false,
        promptNonce: winner.value.promptNonce,
        promptAuthorizedAt: winner.value.promptAuthorizedAt,
      });
      await expect(
        peer.authorizePaymentPrompt(attempt.id, githubId, winner.value.promptNonce!),
      ).resolves.toMatchObject({
        promptNonce: winner.value.promptNonce,
        promptAuthorizedAt: winner.value.promptAuthorizedAt,
      });
    } finally {
      await peer.close();
    }
  });

  it('admits only one cross-account wallet prompt against one job of capacity', async () => {
    const peer = await PostgresStore.connect(databaseUrl!);
    const firstGithubId = randomUUID();
    const secondGithubId = randomUUID();
    const firstQuote = await saveQuote(store);
    const secondQuote = await saveQuote(peer);
    await Promise.all([
      store.upsertContributor(firstGithubId, 'first-capacity-maintainer'),
      peer.upsertContributor(secondGithubId, 'second-capacity-maintainer'),
    ]);
    await Promise.all([
      store.linkQuoteToAccount(firstQuote.id, firstGithubId),
      peer.linkQuoteToAccount(secondQuote.id, secondGithubId),
    ]);
    const activeAttempts = await store.paymentAttemptCapacity();
    const unfinishedJobs = (await store.jobsList()).filter(
      (job) => job.state !== 'delivered' && job.state !== 'payment_expired',
    );
    const unfinishedWithoutLiability = unfinishedJobs.filter((job) => !job.refundLiabilityId);
    const baselineRefundRaw = unfinishedWithoutLiability.reduce(
      (total, job) => total + BigInt(job.payment.amountAtomic),
      BigInt(activeAttempts.refundRaw),
    );
    const baselineRefundTransactions =
      unfinishedWithoutLiability.length + activeAttempts.refundTransactions;
    const baselineBountyUsdCents = unfinishedJobs.reduce(
      (total, job) =>
        total + calculateRescueBountyPriceCents(Number(BigInt(job.quote.priceAtomic) / 10_000n)),
      activeAttempts.bountyUsdCents,
    );
    const readiness = {
      healthy: true,
      refundTreasury: 'refund-treasury',
      refundMint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
      refundDecimals: 6,
      finalizedBalanceRaw: (baselineRefundRaw + 2_000_000n).toString(),
      pendingRefundRaw: '0',
      treasuryAvailableRefundRaw: (baselineRefundRaw + 2_000_000n).toString(),
      remainingRefundLimitUsdCents: 1_000_000,
      availableRefundRaw: (baselineRefundRaw + 2_000_000n).toString(),
      availableRefundTransactions: baselineRefundTransactions + 1,
      remainingEscrowLimitUsdCents: baselineBountyUsdCents + 1_000,
      escrowAuthority: 'escrow-authority',
      finalizedEscrowBalanceLamports: '1000000',
      availableEscrowReserveLamports: '1000000',
    };
    const config = {
      payTo: 'refund-treasury',
      escrowRefundTo: 'escrow-authority',
      escrowReadinessMinLamports: 1_000_000,
    };
    const policy = { readiness: async () => readiness };
    const admit = (candidate: PostgresStore, githubId: string, quote: Quote) =>
      candidate.withAdmissionLock(async () => {
        await ensurePaymentCapacity(
          { config, store: candidate, policy } as never,
          BigInt(quote.priceAtomic),
          1_000,
        );
        const attempt = await candidate.createPaymentAttempt({
          githubId,
          quoteId: quote.id,
          wallet: '1'.repeat(32),
          appBuild: 'release-test',
        });
        return candidate.authorizePaymentPrompt(attempt.id, githubId, randomUUID());
      });

    try {
      const results = await Promise.allSettled([
        admit(store, firstGithubId, firstQuote),
        admit(peer, secondGithubId, secondQuote),
      ]);
      expect(results.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
      expect(
        results.some(
          (result) => result.status === 'rejected' && result.reason instanceof RefundCapacityError,
        ),
      ).toBe(true);
      await expect(store.paymentAttemptCapacity()).resolves.toEqual({
        refundRaw: (BigInt(activeAttempts.refundRaw) + 2_000_000n).toString(),
        refundTransactions: activeAttempts.refundTransactions + 1,
        bountyUsdCents: activeAttempts.bountyUsdCents + 1_000,
      });
    } finally {
      await peer.close();
    }
  });

  it('serializes and records schema migration reruns', async () => {
    const [left, right] = await Promise.all([
      PostgresStore.connect(databaseUrl!),
      PostgresStore.connect(databaseUrl!),
    ]);
    const pool = new Pool({ connectionString: databaseUrl });
    try {
      const result = await pool.query<{
        component: string;
        version: number;
        name: string;
        checksum: string;
      }>(
        `SELECT component, version, name, checksum FROM mizuki_schema_migrations
         WHERE component IN (
           'core', 'admission-control', 'github-oauth', 'social', 'workbench',
           'workbench-api-tokens'
         )
         ORDER BY component, version`,
      );
      expect(result.rows).toMatchObject([
        { component: 'admission-control', version: 1, name: 'admission-control-audit' },
        { component: 'core', version: 1, name: 'commercial-core' },
        { component: 'github-oauth', version: 1, name: 'browser-bound-flow' },
        { component: 'social', version: 1, name: 'social-post-receipts' },
        { component: 'workbench', version: 1, name: 'workbench-accounts' },
        { component: 'workbench', version: 2, name: 'payment-attempts' },
        { component: 'workbench-api-tokens', version: 1, name: 'scoped-api-tokens' },
      ]);
      expect(result.rows.every((row) => /^[a-f0-9]{64}$/.test(row.checksum))).toBe(true);
    } finally {
      await Promise.all([left.close(), right.close(), pool.end()]);
    }
  });

  it('atomically consumes a browser-bound OAuth flow once', async () => {
    const flow = {
      id: randomUUID(),
      binding: 'a'.repeat(43),
      expiresAt: new Date(Date.now() + 60_000).toISOString(),
      createdAt: new Date().toISOString(),
    };
    await store.saveGithubOAuthFlow(flow);

    const callbacks = await Promise.allSettled([
      store.consumeGithubOAuthFlow(flow.id, flow.binding),
      store.consumeGithubOAuthFlow(flow.id, flow.binding),
    ]);

    expect(callbacks.filter((callback) => callback.status === 'fulfilled')).toHaveLength(1);
    expect(
      callbacks.some(
        (callback) =>
          callback.status === 'rejected' && callback.reason instanceof StateConflictError,
      ),
    ).toBe(true);
  });

  it('caps pending browser OAuth flows across runtime replicas', async () => {
    const pool = new Pool({ connectionString: databaseUrl });
    const now = new Date();
    const expiresAt = new Date(now.getTime() + 60_000);
    const flows = Array.from({ length: MAX_PENDING_GITHUB_OAUTH_FLOWS }, () => randomUUID());
    const values = flows.map(
      (_, index) => `($${index * 4 + 1}, $${index * 4 + 2}, $${index * 4 + 3}, $${index * 4 + 4})`,
    );
    const parameters = flows.flatMap((id) => [id, 'a'.repeat(43), expiresAt, now]);
    const replacement = {
      id: randomUUID(),
      binding: 'b'.repeat(43),
      expiresAt: expiresAt.toISOString(),
      createdAt: now.toISOString(),
    };

    try {
      await pool.query('DELETE FROM mizuki_github_oauth_flows');
      await pool.query(
        `INSERT INTO mizuki_github_oauth_flows (id, binding, expires_at, created_at)
         VALUES ${values.join(', ')}`,
        parameters,
      );

      await expect(store.saveGithubOAuthFlow(replacement)).rejects.toBeInstanceOf(
        GithubOAuthCapacityError,
      );
      await pool.query('UPDATE mizuki_github_oauth_flows SET consumed_at = now() WHERE id = $1', [
        flows[0],
      ]);
      await expect(store.saveGithubOAuthFlow(replacement)).resolves.toEqual(replacement);
      const count = await pool.query<{ count: string }>(
        'SELECT count(*)::text AS count FROM mizuki_github_oauth_flows',
      );
      expect(Number(count.rows[0]?.count)).toBe(MAX_PENDING_GITHUB_OAUTH_FLOWS);
    } finally {
      await pool.query('DELETE FROM mizuki_github_oauth_flows');
      await pool.end();
    }
  });

  it('serializes the repository cap per account and keeps listing bounded', async () => {
    const githubId = randomUUID();
    await store.upsertContributor(githubId, 'repository-cap-maintainer');
    await Promise.all(
      Array.from({ length: 24 }, (_, index) =>
        store.linkAccountRepository(githubId, 'example', `project-${index}`),
      ),
    );

    const boundary = await Promise.allSettled([
      store.linkAccountRepository(githubId, 'example', 'project-24'),
      store.linkAccountRepository(githubId, 'example', 'project-25'),
    ]);
    expect(boundary.filter((result) => result.status === 'fulfilled')).toHaveLength(1);
    expect(
      boundary.some(
        (result) =>
          result.status === 'rejected' &&
          result.reason instanceof StateConflictError &&
          result.reason.message === 'account repository limit of 25 reached',
      ),
    ).toBe(true);
    await expect(
      store.linkAccountRepository(githubId, 'Example', 'project-0'),
    ).resolves.toMatchObject({ repository: 'example/project-0', owner: 'Example' });

    const bounded = await store.repositoriesForAccount(githubId, 10);
    expect(bounded.repositories).toHaveLength(10);
    expect(bounded).toMatchObject({ limit: 10, truncated: true });
    const complete = await store.repositoriesForAccount(githubId, 25);
    expect(complete.repositories).toHaveLength(25);
    expect(complete).toMatchObject({ limit: 25, truncated: false });
  });

  it('filters and bounds account bounty history in PostgreSQL', async () => {
    const quote = await saveQuote(store);
    const { job } = await store.createJob(
      quote,
      { payer: '7'.repeat(32), transaction: randomUUID(), amountAtomic: '2000000' },
      `account-bounties-${randomUUID()}`,
    );
    const githubId = randomUUID();
    const active = {
      ...bounty(job.id, quote, 0),
      state: 'open' as const,
      activeClaim: accountClaim(githubId, 'active'),
    };
    const historical = {
      ...bounty(job.id, quote, 1),
      state: 'open' as const,
      claimHistory: [accountClaim(githubId, 'released')],
    };
    const unrelated = {
      ...bounty(job.id, quote, 2),
      state: 'open' as const,
      activeClaim: accountClaim(randomUUID(), 'active'),
    };
    await Promise.all([
      store.createBounty(active),
      store.createBounty(historical),
      store.createBounty(unrelated),
    ]);

    const bounded = await store.bountiesForAccount(githubId, 1);
    expect(bounded.bounties).toHaveLength(1);
    expect(bounded).toMatchObject({ limit: 1, truncated: true });
    const complete = await store.bountiesForAccount(githubId, 100);
    expect(new Set(complete.bounties.map(({ bounty }) => bounty.id))).toEqual(
      new Set([active.id, historical.id]),
    );
    expect(complete.bounties.every(({ claim }) => claim.claimantId === githubId)).toBe(true);
    await expect(store.bountyForAccount(active.id, githubId)).resolves.toMatchObject({
      bounty: { id: active.id },
      claim: { id: active.activeClaim.id },
    });
    await expect(store.bountyForAccount(unrelated.id, githubId)).resolves.toBeUndefined();
    expect(complete).toMatchObject({ limit: 100, truncated: false });
  });

  it('upgrades the exact v1 schema to an append-only admission ledger', async () => {
    const schema = `mizuki_upgrade_${randomUUID().replaceAll('-', '')}`;
    const adminPool = new Pool({ connectionString: databaseUrl });
    const upgradeUrl = new URL(databaseUrl!);
    upgradeUrl.searchParams.set('options', `-c search_path=${schema}`);
    let upgraded: PostgresStore | undefined;

    await adminPool.query(`CREATE SCHEMA ${schema}`);
    try {
      const v1Pool = new Pool({ connectionString: upgradeUrl.toString() });
      try {
        expect(createHash('sha256').update(COMMERCIAL_CORE_SCHEMA_V1).digest('hex')).toBe(
          DEPLOYED_CORE_V1_CHECKSUM,
        );
        await v1Pool.query(COMMERCIAL_CORE_SCHEMA_V1);
        await v1Pool.query(`
          CREATE TABLE mizuki_schema_migrations (
            component text NOT NULL,
            version integer NOT NULL CHECK (version > 0),
            name text NOT NULL,
            checksum text NOT NULL CHECK (checksum ~ '^[a-f0-9]{64}$'),
            applied_at timestamptz NOT NULL DEFAULT now(),
            PRIMARY KEY (component, version)
          )
        `);
        await v1Pool.query(
          `INSERT INTO mizuki_schema_migrations (component, version, name, checksum)
           VALUES ('core', 1, 'commercial-core', $1)`,
          [DEPLOYED_CORE_V1_CHECKSUM],
        );
        await v1Pool.query(
          `UPDATE mizuki_operator_controls
           SET revision = 4, reason = 'closed v1 production snapshot', updated_by = 'operator'
           WHERE singleton = true`,
        );
      } finally {
        await v1Pool.end();
      }

      upgraded = await PostgresStore.connect(upgradeUrl.toString());
      await expect(upgraded.operatorControlsAudit()).resolves.toEqual([
        expect.objectContaining({
          revision: 4,
          expectedRevision: 4,
          intakeEnabled: false,
          claimsEnabled: false,
          reason: 'closed v1 production snapshot',
        }),
      ]);
      await expect(
        upgraded.updateOperatorControls({
          expectedRevision: 4,
          intakeEnabled: false,
          claimsEnabled: false,
          reason: 'first revision-bound v2 mutation',
          updatedBy: 'operator',
        }),
      ).resolves.toMatchObject({ revision: 5, intakeEnabled: false, claimsEnabled: false });
      await expect(upgraded.operatorControlsAudit()).resolves.toHaveLength(2);

      const githubId = randomUUID();
      await upgraded.upsertContributor(githubId, 'migration-maintainer');
      const quote = await saveQuote(upgraded);
      await upgraded.linkQuoteToAccount(quote.id, githubId);
      await expect(upgraded.quoteForAccount(quote.id, githubId)).resolves.toEqual(quote);
      await expect(upgraded.quoteForAccount(quote.id, randomUUID())).resolves.toBeUndefined();
      const repository = await upgraded.linkAccountRepository(githubId, quote.owner, quote.repo);
      const { job } = await upgraded.createJob(
        quote,
        { payer: '5'.repeat(32), transaction: randomUUID(), amountAtomic: '2000000' },
        `migration-${randomUUID()}`,
      );
      await expect(upgraded.jobsForAccount(githubId, 100)).resolves.toEqual({
        jobs: [job],
        limit: 100,
        truncated: false,
        obligationCount: 1,
      });
      await upgraded.transitionJob(job.id, 'settlement_pending', 'delivered', {
        refundLiabilityId: 'postgres-liability-1',
      });
      await expect(upgraded.jobsForAccount(githubId, 100)).resolves.toMatchObject({
        jobs: [expect.objectContaining({ id: job.id })],
        obligationCount: 1,
      });
      await upgraded.patchJob(job.id, {
        refundLiabilityDischargedAt: '2026-08-25T05:00:00.000Z',
      });
      await expect(upgraded.jobsForAccount(githubId, 100)).resolves.toMatchObject({
        jobs: [expect.objectContaining({ id: job.id })],
        obligationCount: 0,
      });
      const secondQuote = await saveQuote(upgraded);
      await upgraded.linkQuoteToAccount(secondQuote.id, githubId);
      await upgraded.createJob(
        secondQuote,
        { payer: '6'.repeat(32), transaction: randomUUID(), amountAtomic: '2000000' },
        `migration-second-${randomUUID()}`,
      );
      const bounded = await upgraded.jobsForAccount(githubId, 1);
      expect(bounded.jobs).toHaveLength(2);
      expect(bounded).toMatchObject({ limit: 1, truncated: false, obligationCount: 1 });
      const complete = await upgraded.jobsForAccount(githubId, 100);
      expect(complete.jobs).toHaveLength(2);
      expect(complete).toMatchObject({ limit: 100, truncated: false, obligationCount: 1 });
      await expect(upgraded.repositoriesForAccount(githubId, 25)).resolves.toEqual({
        repositories: [repository],
        limit: 25,
        truncated: false,
      });

      const verificationPool = new Pool({ connectionString: upgradeUrl.toString() });
      try {
        await expect(
          verificationPool.query(
            'DELETE FROM mizuki_operator_control_audit WHERE revision = $1',
            [4],
          ),
        ).rejects.toThrow('operator control audit is append-only');
        await expect(
          verificationPool.query('TRUNCATE mizuki_operator_control_audit'),
        ).rejects.toThrow('operator control audit is append-only');
        const migrations = await verificationPool.query<{
          component: string;
          version: number;
          name: string;
        }>(
          `SELECT component, version, name FROM mizuki_schema_migrations
           WHERE component IN (
             'core', 'admission-control', 'github-oauth', 'social', 'workbench',
             'workbench-api-tokens'
           )
           ORDER BY component, version`,
        );
        expect(migrations.rows).toEqual([
          { component: 'admission-control', version: 1, name: 'admission-control-audit' },
          { component: 'core', version: 1, name: 'commercial-core' },
          { component: 'github-oauth', version: 1, name: 'browser-bound-flow' },
          { component: 'social', version: 1, name: 'social-post-receipts' },
          { component: 'workbench', version: 1, name: 'workbench-accounts' },
          { component: 'workbench', version: 2, name: 'payment-attempts' },
          { component: 'workbench-api-tokens', version: 1, name: 'scoped-api-tokens' },
        ]);
      } finally {
        await verificationPool.end();
      }
    } finally {
      await upgraded?.close();
      await adminPool.query(`DROP SCHEMA ${schema} CASCADE`);
      await adminPool.end();
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

function accountClaim(claimantId: string, state: BountyClaim['state']): BountyClaim {
  const at = '2026-08-25T00:00:00.000Z';
  return {
    id: randomUUID(),
    claimantId,
    walletAddress: '1'.repeat(32),
    state,
    claimedAt: at,
    leaseExpiresAt: '2026-08-27T00:00:00.000Z',
    ...(state === 'active' ? {} : { closedAt: at }),
  };
}

function socialReceipt(): SocialPostReceipt {
  return {
    id: randomUUID(),
    kind: 'stats',
    cursor: `stats:${randomUUID()}`,
    sourceHash: randomUUID().replaceAll('-', '').repeat(2),
    postId: randomUUID(),
    text: 'Internal test activity stayed separate from external work.',
    snapshot: {
      internalPaidAttempts: 1,
      externalPaidJobs: 0,
      unclassifiedPaidAttempts: 0,
      internalOpenedPrs: 1,
      externalOpenedPrs: 0,
      unclassifiedOpenedPrs: 0,
      internalMergedPrs: 1,
      externalMergedPrs: 0,
      unclassifiedMergedPrs: 0,
      internalRefunds: 0,
      externalRefunds: 0,
      unclassifiedRefunds: 0,
      refundSuccessRate: null,
      externalMaintainers: 0,
      grossMarginStatus: 'unverified',
    },
    postedAt: new Date().toISOString(),
  };
}
