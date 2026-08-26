import { randomUUID } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { loadConfig } from './config.js';
import type { ServiceReadinessReport } from './readiness.js';
import { buildSocialBrief, snapshotFromSocialBrief } from './social.js';
import { MemoryStore } from './store.js';
import type { GithubAuthorizationReceipt, Quote, SocialPostReceipt } from './types.js';

const at = new Date('2026-08-25T16:00:00.000Z');

describe('social brief', () => {
  it('separates internal tests from external paid work and emits deltas', async () => {
    const store = new MemoryStore();
    await mergedJob(store, quote('internal', false));
    await refundedJob(store, quote('external', true));
    const brief = await buildSocialBrief(config(), store, readiness(), 'stats', at);

    expect(brief).toMatchObject({
      schemaVersion: 1,
      kind: 'stats',
      publishable: true,
      metrics: {
        internalPaidAttempts: { total: 1, delta: 1 },
        externalPaidJobs: { total: 1, delta: 1 },
        unclassifiedPaidAttempts: { total: 0, delta: 0 },
        internalMergedPrs: { total: 1, delta: 1 },
        externalRefunds: { total: 1, delta: 1 },
        refundSuccessRate: 1,
        externalMaintainers: { total: 1, delta: 1 },
        grossMarginStatus: 'unverified',
      },
      blockedReasons: [],
      reviewRequiredReasons: ['financial_metrics'],
    });
    expect(brief.sourceHash).toMatch(/^[a-f0-9]{64}$/);
    expect(brief.evidence.some(({ url }) => url.includes('/api/mizuki/v1/social/brief'))).toBe(
      true,
    );
    expect(brief.evidence.some(({ url }) => url.includes('github.com/internal/tool/pull/1'))).toBe(
      true,
    );
  });

  it('blocks ambiguous paid activity instead of calling it internal', async () => {
    const store = new MemoryStore();
    await mergedJob(store, quote('unknown', false));

    const brief = await buildSocialBrief(config(), store, readiness(), 'stats', at);

    expect(brief.publishable).toBe(false);
    expect(brief.metrics.unclassifiedPaidAttempts.total).toBe(1);
    expect(brief.blockedReasons).toContain('unclassified_paid_activity');
  });

  it('blocks a repeated source after the confirmed post receipt is stored', async () => {
    const store = new MemoryStore();
    await mergedJob(store, quote('internal', false));
    const first = await buildSocialBrief(config(), store, readiness(), 'stats', at);
    await store.saveSocialPost(receipt(first));

    const repeated = await buildSocialBrief(config(), store, readiness(), 'stats', at);

    expect(repeated.publishable).toBe(false);
    expect(repeated.metrics.internalPaidAttempts.delta).toBe(0);
    expect(repeated.blockedReasons).toEqual(
      expect.arrayContaining(['duplicate_source', 'no_changes_since_last_post']),
    );
  });

  it('blocks stats while refund protection is unavailable', async () => {
    const store = new MemoryStore();
    await mergedJob(store, quote('internal', false));

    const brief = await buildSocialBrief(
      config(),
      store,
      { ...readiness(), ready: false },
      'stats',
      at,
    );

    expect(brief.publishable).toBe(false);
    expect(brief.blockedReasons).toContain('refund_protection_unverified');
  });

  it('blocks a completion whose evidence URL is outside the allowlist', async () => {
    const store = new MemoryStore();
    const job = await mergedJob(store, quote('internal', false));
    await store.patchJob(job.id, { prUrl: 'https://untrusted.example/pull/1' });

    const brief = await buildSocialBrief(config(), store, readiness(), 'stats', at);

    expect(brief.evidence.some(({ claim }) => claim === 'mergedPr')).toBe(false);
    expect(brief.blockedReasons).toContain('completion_evidence_unavailable');
  });

  it('detects counter regression against the durable receipt', async () => {
    const store = new MemoryStore();
    await mergedJob(store, quote('internal', false));
    const current = await buildSocialBrief(config(), store, readiness(), 'stats', at);
    const previous = receipt(current);
    previous.sourceHash = 'b'.repeat(64);
    previous.cursor = 'stats:older';
    previous.snapshot.internalPaidAttempts = 2;
    await store.saveSocialPost(previous);

    const regressed = await buildSocialBrief(config(), store, readiness(), 'stats', at);

    expect(regressed.metrics.internalPaidAttempts.delta).toBe(-1);
    expect(regressed.blockedReasons).toContain('counter_regression');
  });
});

function config() {
  return loadConfig({
    MIZUKI_PAYMENT_MODE: 'mock',
    MIZUKI_INTERNAL_REPOS: 'internal/tool',
    MIZUKI_PUBLIC_BASE_URL: 'https://api.example.test',
    MIZUKI_WEB_ORIGIN: 'https://mizuki.example.test',
  });
}

function quote(owner: string, external: boolean): Quote {
  const authorization: GithubAuthorizationReceipt = {
    label: 'mizuki:authorized',
    actorId: 'maintainer-1',
    actorLogin: 'maintainer',
    permission: 'maintain',
    authorizedAt: at.toISOString(),
    verifiedAt: at.toISOString(),
    evidenceHash: 'e'.repeat(64),
  };
  return {
    id: randomUUID(),
    issueUrl: `https://github.com/${owner}/tool/issues/1`,
    owner,
    repo: 'tool',
    issueNumber: 1,
    issueTitle: 'Fix bounded issue',
    issueBody: '',
    baseSha: 'a'.repeat(40),
    defaultBranch: 'main',
    ...(external ? { installationId: 42, authorizationReceipt: authorization } : {}),
    class: 'micro',
    priceAtomic: '2000000',
    maxFiles: 3,
    maxCostUsd: 0.8,
    validationCommands: [],
    expiresAt: '2099-01-01T00:00:00.000Z',
  };
}

async function mergedJob(store: MemoryStore, value: Quote) {
  const { job } = await store.createJob(
    value,
    { payer: 'payer', transaction: randomUUID(), amountAtomic: value.priceAtomic },
    randomUUID(),
  );
  return store.transitionJob(job.id, 'settlement_pending', 'delivered', {
    prUrl: `https://github.com/${value.owner}/${value.repo}/pull/1`,
    mergedAt: at.toISOString(),
    refundLiabilityId: randomUUID(),
    refundLiabilityDischargedAt: at.toISOString(),
  });
}

async function refundedJob(store: MemoryStore, value: Quote) {
  const { job } = await store.createJob(
    value,
    { payer: 'payer', transaction: randomUUID(), amountAtomic: value.priceAtomic },
    randomUUID(),
  );
  return store.transitionJob(job.id, 'settlement_pending', 'refunded', {
    refundTransaction: randomUUID(),
  });
}

function receipt(brief: Awaited<ReturnType<typeof buildSocialBrief>>): SocialPostReceipt {
  return {
    id: randomUUID(),
    kind: 'stats',
    cursor: brief.cursor,
    sourceHash: brief.sourceHash,
    postId: '1234567890',
    text: 'Internal test receipt.',
    snapshot: snapshotFromSocialBrief(brief),
    postedAt: at.toISOString(),
  };
}

function readiness(): ServiceReadinessReport {
  const checkedAt = at.toISOString();
  return {
    ready: true,
    checkedAt,
    ageMs: 0,
    lastSuccessfulAt: checkedAt,
    lastSuccessfulAgeMs: 0,
    failed: [],
    dependencies: {
      configuration: { ok: true, checkedAt, latencyMs: 1 },
      postgres: { ok: true, checkedAt, latencyMs: 1 },
      operator_controls: { ok: true, checkedAt, latencyMs: 1 },
      coding_gateway: { ok: true, checkedAt, latencyMs: 1 },
      github_app: { ok: true, checkedAt, latencyMs: 1 },
      reviewer_route: { ok: true, checkedAt, latencyMs: 1 },
      updater: { ok: true, checkedAt, latencyMs: 1 },
      x402_facilitator: { ok: true, checkedAt, latencyMs: 1 },
      policy_signer: {
        ok: true,
        checkedAt,
        latencyMs: 1,
        refundProtection: {
          refundTreasury: 'refund-treasury',
          refundMint: 'usdc-mint',
          refundDecimals: 6,
          finalizedBalanceRaw: '100000000',
          pendingRefundRaw: '0',
          treasuryAvailableRefundRaw: '100000000',
          remainingRefundLimitUsdCents: 10_000,
          availableRefundRaw: '100000000',
          escrowAuthority: 'escrow-authority',
          finalizedEscrowBalanceLamports: '2000000000',
          availableEscrowReserveLamports: '1900000000',
        },
      },
    },
  };
}
