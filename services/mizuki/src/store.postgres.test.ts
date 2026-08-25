import { createHash, randomUUID } from 'node:crypto';
import { Pool } from 'pg';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  type BountyClaim,
  createContributorEscrow,
  createRescueBounty,
  transitionContributorEscrow,
} from './domain/index.js';
import {
  COMMERCIAL_CORE_SCHEMA_V1,
  GithubOAuthCapacityError,
  MAX_PENDING_GITHUB_OAUTH_FLOWS,
  PostgresStore,
  StateConflictError,
} from './store.js';
import type { Quote } from './types.js';

const databaseUrl = process.env.MIZUKI_TEST_DATABASE_URL;
const DEPLOYED_CORE_V1_CHECKSUM =
  '1e1c7b752aead2d673a8d82fba69113344ada76444a1263e6bc80bffb0d80429';

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
         WHERE component IN ('core', 'admission-control', 'github-oauth', 'workbench')
         ORDER BY component, version`,
      );
      expect(result.rows).toMatchObject([
        { component: 'admission-control', version: 1, name: 'admission-control-audit' },
        { component: 'core', version: 1, name: 'commercial-core' },
        { component: 'github-oauth', version: 1, name: 'browser-bound-flow' },
        { component: 'workbench', version: 1, name: 'workbench-accounts' },
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
    expect(new Set(complete.bounties.map((candidate) => candidate.id))).toEqual(
      new Set([active.id, historical.id]),
    );
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
      });
      const secondQuote = await saveQuote(upgraded);
      await upgraded.linkQuoteToAccount(secondQuote.id, githubId);
      await upgraded.createJob(
        secondQuote,
        { payer: '6'.repeat(32), transaction: randomUUID(), amountAtomic: '2000000' },
        `migration-second-${randomUUID()}`,
      );
      const bounded = await upgraded.jobsForAccount(githubId, 1);
      expect(bounded.jobs).toHaveLength(1);
      expect(bounded).toMatchObject({ limit: 1, truncated: true });
      const complete = await upgraded.jobsForAccount(githubId, 100);
      expect(complete.jobs).toHaveLength(2);
      expect(complete).toMatchObject({ limit: 100, truncated: false });
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
           WHERE component IN ('core', 'admission-control', 'github-oauth', 'workbench')
           ORDER BY component, version`,
        );
        expect(migrations.rows).toEqual([
          { component: 'admission-control', version: 1, name: 'admission-control-audit' },
          { component: 'core', version: 1, name: 'commercial-core' },
          { component: 'github-oauth', version: 1, name: 'browser-bound-flow' },
          { component: 'workbench', version: 1, name: 'workbench-accounts' },
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
