import { describe, expect, it } from 'vitest';
import { MemoryStore } from './store.js';
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
