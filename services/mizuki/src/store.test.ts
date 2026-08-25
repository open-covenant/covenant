import { describe, expect, it } from 'vitest';
import {
  API_TOKEN_MAX_ACTIVE,
  API_TOKEN_TERMINAL_HISTORY_LIMIT,
  ApiTokenCapacityError,
  createApiToken,
} from './api-tokens.js';
import { createRescueBounty, type BountyClaim, type RescueBounty } from './domain/index.js';
import { GithubOAuthCapacityError, MAX_PENDING_GITHUB_OAUTH_FLOWS, MemoryStore } from './store.js';
import type { Payment, Quote } from './types.js';

const quote: Quote = {
  id: 'quote-1',
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
  expiresAt: '2099-01-01T00:00:00Z',
};
const payment: Payment = { payer: '1'.repeat(32), transaction: 'tx', amountAtomic: '2000000' };

describe('MemoryStore', () => {
  it('keeps every active API token discoverable after terminal history exceeds the page limit', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    const startedAt = Date.now();
    const active = createApiToken({
      githubId: '42',
      name: 'Long-lived MCP',
      scopes: ['repositories:read'],
      expiresAt: new Date(startedAt + 30 * 24 * 60 * 60_000).toISOString(),
      now: new Date(startedAt),
    });
    await store.createApiToken(active.record);

    for (let index = 1; index <= 105; index += 1) {
      const createdAt = new Date(startedAt + index * 1_000);
      const terminal = createApiToken({
        githubId: '42',
        name: `Terminal MCP ${index}`,
        scopes: ['jobs:read'],
        expiresAt: new Date(createdAt.getTime() + 24 * 60 * 60_000).toISOString(),
        now: createdAt,
      });
      await store.createApiToken(terminal.record);
      await store.revokeApiToken(
        terminal.record.id,
        '42',
        new Date(createdAt.getTime() + 1).toISOString(),
      );
    }

    const tokens = await store.apiTokensForAccount('42');
    expect(tokens).toHaveLength(API_TOKEN_TERMINAL_HISTORY_LIMIT + 1);
    expect(tokens[0]?.id).toBe(active.record.id);
  });

  it('keeps API tokens account-bound, revocable, and capped', async () => {
    const store = new MemoryStore();
    await Promise.all([
      store.upsertContributor('42', 'maintainer'),
      store.upsertContributor('99', 'other-maintainer'),
    ]);
    const records = [];
    for (let index = 0; index < API_TOKEN_MAX_ACTIVE; index += 1) {
      const credential = createApiToken({
        githubId: '42',
        name: `MCP ${index + 1}`,
        scopes: ['repositories:read'],
        expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
      });
      records.push(await store.createApiToken(credential.record));
    }
    expect(await store.apiTokensForAccount('42')).toHaveLength(API_TOKEN_MAX_ACTIVE);
    expect(await store.apiTokensForAccount('99')).toEqual([]);

    const overflow = createApiToken({
      githubId: '42',
      name: 'Overflow',
      scopes: ['jobs:read'],
      expiresAt: new Date(Date.now() + 30 * 24 * 60 * 60_000).toISOString(),
    });
    await expect(store.createApiToken(overflow.record)).rejects.toBeInstanceOf(
      ApiTokenCapacityError,
    );

    const first = records[0]!;
    const usedAt = new Date(Date.now() + 1_000).toISOString();
    await expect(store.markApiTokenUsed(first.id, usedAt)).resolves.toBe(true);
    await expect(
      store.markApiTokenUsed(first.id, new Date(Date.parse(usedAt) - 500).toISOString()),
    ).resolves.toBe(true);
    expect((await store.apiTokenByPrefix(first.prefix))?.lastUsedAt).toBe(usedAt);
    await expect(
      store.revokeApiToken(first.id, '99', new Date().toISOString()),
    ).resolves.toBeUndefined();
    const revoked = await store.revokeApiToken(first.id, '42', new Date().toISOString());
    expect(revoked?.revokedAt).toBeTruthy();
    await expect(store.markApiTokenUsed(first.id, new Date().toISOString())).resolves.toBe(false);
    await expect(store.createApiToken(overflow.record)).resolves.toMatchObject({
      id: overflow.record.id,
    });
  });

  it('bounds pending browser OAuth flows and reclaims terminal entries', async () => {
    const store = new MemoryStore();
    const expiresAt = new Date(Date.now() + 60_000).toISOString();
    for (let index = 0; index < MAX_PENDING_GITHUB_OAUTH_FLOWS; index += 1) {
      await store.saveGithubOAuthFlow({
        id: `00000000-0000-4000-8000-${String(index).padStart(12, '0')}`,
        binding: 'a'.repeat(43),
        expiresAt,
        createdAt: new Date().toISOString(),
      });
    }

    await expect(
      store.saveGithubOAuthFlow({
        id: '10000000-0000-4000-8000-000000000000',
        binding: 'b'.repeat(43),
        expiresAt,
        createdAt: new Date().toISOString(),
      }),
    ).rejects.toBeInstanceOf(GithubOAuthCapacityError);

    await store.consumeGithubOAuthFlow('00000000-0000-4000-8000-000000000000', 'a'.repeat(43));
    await expect(
      store.saveGithubOAuthFlow({
        id: '10000000-0000-4000-8000-000000000000',
        binding: 'b'.repeat(43),
        expiresAt,
        createdAt: new Date().toISOString(),
      }),
    ).resolves.toMatchObject({ id: '10000000-0000-4000-8000-000000000000' });
  });

  it('starts with paid intake and claims closed and updates them durably for the store lifetime', async () => {
    const store = new MemoryStore();
    await expect(store.operatorControls()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });
    const opened = await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'operator completed launch checks',
      updatedBy: 'operator',
    });
    expect(opened).toMatchObject({ intakeEnabled: true, claimsEnabled: true, revision: 1 });
    await expect(store.operatorControls()).resolves.toEqual(opened);
  });

  it('rejects a stale reopen, lets an emergency close win, and retains every revision', async () => {
    const store = new MemoryStore();
    const [opened, closed] = await Promise.all([
      store.updateOperatorControls({
        expectedRevision: 0,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'open one bounded canary window',
        updatedBy: 'operator',
      }),
      store.updateOperatorControls({
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'emergency closure overrides an in-flight open',
        updatedBy: 'operator',
      }),
    ]);

    expect(opened).toMatchObject({ revision: 1, intakeEnabled: true, claimsEnabled: true });
    expect(closed).toMatchObject({ revision: 2, intakeEnabled: false, claimsEnabled: false });
    await expect(
      store.updateOperatorControls({
        expectedRevision: 1,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'delayed retry from the previous open request',
        updatedBy: 'operator',
      }),
    ).rejects.toThrow('expected operator admission revision 1; current revision is 2');
    await expect(store.operatorControls()).resolves.toEqual(closed);
    await expect(store.operatorControlsAudit()).resolves.toEqual([
      expect.objectContaining({
        revision: 0,
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: false,
      }),
      expect.objectContaining({ ...opened, expectedRevision: 0 }),
      expect.objectContaining({ ...closed, expectedRevision: 0 }),
    ]);
  });

  it('deduplicates a paid job by idempotency key', async () => {
    const store = new MemoryStore();
    const first = await store.createJob(quote, payment, 'same-key');
    const second = await store.createJob(quote, payment, 'same-key');
    expect(first.created).toBe(true);
    expect(second.created).toBe(false);
    expect(second.job.id).toBe(first.job.id);
    expect(await store.jobsList()).toHaveLength(1);
  });

  it('links account jobs and repositories without changing anonymous job storage', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const repository = await store.linkAccountRepository('42', quote.owner, quote.repo);
    const { job } = await store.createJob(quote, payment, 'account-job');

    await expect(store.quoteForAccount(quote.id, '42')).resolves.toEqual(quote);
    await expect(store.quoteForAccount(quote.id, '99')).resolves.toBeUndefined();
    await expect(store.jobsForAccount('42', 100)).resolves.toEqual({
      jobs: [job],
      limit: 100,
      truncated: false,
      obligationCount: 1,
    });
    await expect(store.jobsForAccount('99', 100)).resolves.toEqual({
      jobs: [],
      limit: 100,
      truncated: false,
      obligationCount: 0,
    });
    await expect(store.repositoriesForAccount('42', 25)).resolves.toEqual({
      repositories: [repository],
      limit: 25,
      truncated: false,
    });
  });

  it('caps linked repositories while allowing an existing link to be refreshed', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await Promise.all(
      Array.from({ length: 25 }, (_, index) =>
        store.linkAccountRepository('42', 'example', `project-${index}`),
      ),
    );

    await expect(store.linkAccountRepository('42', 'Example', 'project-0')).resolves.toMatchObject({
      repository: 'example/project-0',
      owner: 'Example',
    });
    await expect(store.linkAccountRepository('42', 'example', 'project-25')).rejects.toThrow(
      'account repository limit of 25 reached',
    );

    const bounded = await store.repositoriesForAccount('42', 10);
    expect(bounded.repositories).toHaveLength(10);
    expect(bounded).toMatchObject({ limit: 10, truncated: true });
    const complete = await store.repositoriesForAccount('42', 25);
    expect(complete.repositories).toHaveLength(25);
    expect(complete).toMatchObject({ limit: 25, truncated: false });
  });

  it('returns only bounded bounty history for an account', async () => {
    const store = new MemoryStore();
    const active = accountBounty('active-bounty', 'source-active', '42', false);
    const historical = accountBounty('historical-bounty', 'source-historical', '42', true);
    const unrelated = accountBounty('unrelated-bounty', 'source-unrelated', '99', false);
    await Promise.all([
      store.createBounty(active),
      store.createBounty(historical),
      store.createBounty(unrelated),
    ]);

    const bounded = await store.bountiesForAccount('42', 1);
    expect(bounded.bounties).toHaveLength(1);
    expect(bounded).toMatchObject({ limit: 1, truncated: true });
    const complete = await store.bountiesForAccount('42', 100);
    expect(new Set(complete.bounties.map(({ bounty }) => bounty.id))).toEqual(
      new Set([active.id, historical.id]),
    );
    expect(complete.bounties.every(({ claim }) => claim.claimantId === '42')).toBe(true);
    expect(complete).toMatchObject({ limit: 100, truncated: false });
  });

  it('does not allow a quote to be reassigned to another account', async () => {
    const store = new MemoryStore();
    await Promise.all([
      store.upsertContributor('42', 'maintainer'),
      store.upsertContributor('99', 'other-maintainer'),
    ]);
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');

    await expect(store.linkQuoteToAccount(quote.id, '99')).rejects.toThrow(
      'already linked to another account',
    );
  });

  it('bounds account job history queries', async () => {
    const store = new MemoryStore();
    const secondQuote = { ...quote, id: 'quote-2', issueNumber: 2 };
    const activeQuote = { ...quote, id: 'quote-3', issueNumber: 3 };
    await store.upsertContributor('42', 'maintainer');
    await Promise.all([
      store.saveQuote(quote),
      store.saveQuote(secondQuote),
      store.saveQuote(activeQuote),
    ]);
    await Promise.all([
      store.linkQuoteToAccount(quote.id, '42'),
      store.linkQuoteToAccount(secondQuote.id, '42'),
      store.linkQuoteToAccount(activeQuote.id, '42'),
    ]);
    const { job: first } = await store.createJob(quote, payment, 'bounded-job-1');
    const { job: second } = await store.createJob(
      secondQuote,
      { ...payment, transaction: 'tx-2' },
      'bounded-job-2',
    );
    const { job: active } = await store.createJob(
      activeQuote,
      { ...payment, transaction: 'tx-3' },
      'active-job',
    );
    await store.transitionJob(first.id, 'settlement_pending', 'delivered');
    await store.transitionJob(second.id, 'settlement_pending', 'refunded');

    const page = await store.jobsForAccount('42', 1);
    expect(page.jobs).toHaveLength(2);
    expect(page.jobs.map((job) => job.id)).toContain(active.id);
    expect(page).toMatchObject({ limit: 1, truncated: true, obligationCount: 1 });
    await expect(store.jobsForAccount('42', 1_001)).rejects.toThrow('between 1 and 1000');
  });

  it('keeps a delivered job in obligations until its live liability is discharged', async () => {
    const store = new MemoryStore();
    await store.upsertContributor('42', 'maintainer');
    await store.saveQuote(quote);
    await store.linkQuoteToAccount(quote.id, '42');
    const { job } = await store.createJob(quote, payment, 'delivered-liability');
    await store.transitionJob(job.id, 'settlement_pending', 'delivered', {
      refundLiabilityId: 'liability-1',
    });

    await expect(store.jobsForAccount('42', 1)).resolves.toMatchObject({
      jobs: [expect.objectContaining({ id: job.id })],
      obligationCount: 1,
      truncated: false,
    });

    await store.patchJob(job.id, {
      refundLiabilityDischargedAt: '2026-08-25T05:00:00.000Z',
    });
    await expect(store.jobsForAccount('42', 1)).resolves.toMatchObject({
      jobs: [expect.objectContaining({ id: job.id })],
      obligationCount: 0,
      truncated: false,
    });
  });

  it('deduplicates the same payment proof across different idempotency keys', async () => {
    const store = new MemoryStore();
    const paid = { ...payment, signature: 'same-x402-proof' };
    const first = await store.createJob(quote, paid, 'first-key');
    const second = await store.createJob(quote, paid, 'second-key');
    expect(second.created).toBe(false);
    expect(second.job.id).toBe(first.job.id);
  });

  it('reserves one job per quote across different request keys and payments', async () => {
    const store = new MemoryStore();
    const first = await store.createJob(
      quote,
      { ...payment, signature: 'first-payment-proof' },
      'first-request-key',
    );
    const second = await store.createJob(
      quote,
      { ...payment, signature: 'second-payment-proof' },
      'second-request-key',
    );
    expect(second.created).toBe(false);
    expect(second.job.id).toBe(first.job.id);
    expect(await store.jobByQuote(quote.id)).toMatchObject({ id: first.job.id });
  });

  it('rejects an unexpected state transition', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(quote, payment, 'transition-key');
    await expect(store.transitionJob(job.id, 'paid', 'running')).rejects.toThrow('expected paid');
  });

  it('publishes a deterministic activity event exactly once', async () => {
    const store = new MemoryStore();
    const eventId = '11111111-1111-4111-8111-111111111111';
    const first = await store.appendActivity(
      'bounty.disputed',
      'bounty-1',
      { disputeId: 'dispute-1' },
      eventId,
    );
    const replay = await store.appendActivity(
      'bounty.disputed',
      'bounty-1',
      { disputeId: 'dispute-1' },
      eventId,
    );

    expect(replay).toEqual(first);
    expect(await store.activity()).toHaveLength(1);
    await expect(
      store.appendActivity('bounty.disputed', 'bounty-1', { disputeId: 'different' }, eventId),
    ).rejects.toThrow('reused with different values');
  });
});

function accountBounty(
  id: string,
  sourceJobId: string,
  claimantId: string,
  historical: boolean,
): RescueBounty {
  const bounty = createRescueBounty({
    id,
    sourceJobId,
    failureReceiptId: `failure:${id}`,
    repository: 'example/project',
    issueNumber: 1,
    issueUrl: 'https://github.com/example/project/issues/1',
    jobPriceCents: 200,
    at: '2026-08-25T00:00:00.000Z',
  });
  const claim: BountyClaim = {
    id: `claim:${id}`,
    claimantId,
    walletAddress: '1'.repeat(32),
    state: historical ? 'released' : 'active',
    claimedAt: bounty.createdAt,
    leaseExpiresAt: bounty.offerExpiresAt,
    ...(historical ? { closedAt: bounty.updatedAt } : {}),
  };
  return {
    ...bounty,
    state: 'open',
    ...(historical ? { claimHistory: [claim] } : { activeClaim: claim }),
  };
}
