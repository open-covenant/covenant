import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  assertOperatorControlOpen,
  createApp,
  OperatorAdmissionError,
  SerialGate,
  type AppDependencies,
} from './app.js';
import { PolicyRequestError, repositoryAdmissionBinding } from './policy-client.js';
import { MemoryStore } from './store.js';
import type { Quote, RepositoryAdmissionReceipt } from './types.js';

const servers: Server[] = [];

afterEach(async () => {
  await Promise.all(
    servers.splice(0).map(
      (server) =>
        new Promise<void>((resolve, reject) => {
          server.close((cause) => (cause ? reject(cause) : resolve()));
        }),
    ),
  );
});

describe('operator admission controls', () => {
  it('requires authentication to open intake and exposes a public status', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        config: { adminToken: 'admin-secret', paymentMode: 'mock' },
      }),
    );

    const initial = await fetch(`${base}/v1/admission`);
    await expect(initial.json()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });

    const body = JSON.stringify({
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'canary checks completed successfully',
    });
    const unauthorized = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body,
    });
    expect(unauthorized.status).toBe(401);

    const opened = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body,
    });
    expect(opened.status).toBe(200);
    await expect(opened.json()).resolves.toMatchObject({
      intakeEnabled: true,
      claimsEnabled: true,
      revision: 1,
      updatedBy: 'operator',
    });
  });

  it('keeps controls closed while service dependencies are unavailable', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body: JSON.stringify({
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'attempted canary while dependencies are unavailable',
      }),
    });

    expect(response.status).toBe(503);
    await expect(store.operatorControls()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });
  });

  it('never opens admission in the shadow runtime', async () => {
    const store = new MemoryStore();
    const readiness = { check: vi.fn(async () => ({ ready: true })) };
    const base = await serve(
      dependencies(store, {
        config: { runtimeRole: 'shadow' },
        readiness,
      }),
    );

    const response = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body: JSON.stringify({
        intakeEnabled: true,
        reason: 'attempt to open candidate admission',
      }),
    });

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: 'shadow admission is permanently closed',
    });
    expect(readiness.check).not.toHaveBeenCalled();
    await expect(store.operatorControls()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });
  });

  it('does not issue a quote when stale controls are open but readiness is incomplete', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'simulate stale controls from an earlier deployment',
      updatedBy: 'test',
    });
    const issue = vi.fn();
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: { issue },
        payments: { challenge },
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/2' }),
    });

    expect(response.status).toBe(503);
    expect(issue).not.toHaveBeenCalled();
    expect(challenge).not.toHaveBeenCalled();
  });

  it('fails closed before settlement while preserving authoritative idempotent reads', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const settle = vi.fn();
    const deps = dependencies(store, {
      config: { adminToken: 'admin-secret', paymentMode: 'mock' },
      payments: { settle },
      github: {
        assertIssueAuthorization: vi.fn(async () => undefined),
        currentHead: vi.fn(async () => quote.baseSha),
      },
    });
    const base = await serve(deps);

    const blocked = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'job-key' },
      body: JSON.stringify({ quote_id: quote.id }),
    });
    expect(blocked.status).toBe(503);
    await expect(blocked.json()).resolves.toEqual({ error: 'intake is paused by the operator' });
    expect(settle).not.toHaveBeenCalled();

    const reservation = await store.createJob(
      quote,
      { payer: 'payer', transaction: 'pending', amountAtomic: quote.priceAtomic },
      'job-key',
    );
    const replay = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'job-key' },
      body: JSON.stringify({ quote_id: quote.id }),
    });
    expect(replay.status).toBe(200);
    await expect(replay.json()).resolves.toMatchObject({ id: reservation.job.id });
    expect(settle).not.toHaveBeenCalled();
  });

  it('does not accept payment when stale intake is open but readiness is incomplete', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'simulate stale intake from an earlier deployment',
      updatedBy: 'test',
    });
    const settle = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'unready-job' },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(503);
    expect(settle).not.toHaveBeenCalled();
    await expect(store.jobByIdempotencyKey('unready-job')).resolves.toBeUndefined();
  });

  it('probes the exact verifier repository inside the live gate before settlement', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'repository admission ordering test',
      updatedBy: 'test',
    });
    const order: string[] = [];
    const createRepositoryAdmission = vi.fn(async (binding) => {
      order.push('repository');
      return admissionReceipt(binding);
    });
    const settle = vi.fn(async (_quote, _signature, persist) => {
      await persist(authorizedPayment);
      order.push('settle');
      throw new Error('stop after settlement ordering assertion');
    });
    const base = await serve(
      dependencies(store, {
        config: livePaymentConfig,
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        policy: { readiness: vi.fn(async () => signerReadiness), createRepositoryAdmission },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': 'live-ordering-job',
        'payment-signature': 'signed-payment-proof',
      },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(500);
    expect(createRepositoryAdmission).toHaveBeenCalledWith(
      repositoryAdmissionBinding(quote, 'live-ordering-job', 'signed-payment-proof'),
      'signed-payment-proof',
    );
    expect(order).toEqual(['repository', 'settle']);
  });

  it('does not broadcast when exact-repository verifier admission fails', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'repository admission failure test',
      updatedBy: 'test',
    });
    const broadcast = vi.fn();
    const settle = vi.fn(async (_quote, _signature, persist) => {
      await persist(authorizedPayment);
      broadcast();
    });
    const createRepositoryAdmission = vi.fn(async () => {
      throw new Error('policy verifier App is not installed for this repository');
    });
    const base = await serve(
      dependencies(store, {
        config: livePaymentConfig,
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        policy: { readiness: vi.fn(async () => signerReadiness), createRepositoryAdmission },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': 'live-repository-failure',
        'payment-signature': 'signed-payment-proof',
      },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(500);
    expect(createRepositoryAdmission).toHaveBeenCalledWith(
      repositoryAdmissionBinding(quote, 'live-repository-failure', 'signed-payment-proof'),
      'signed-payment-proof',
    );
    expect(settle).toHaveBeenCalledOnce();
    expect(broadcast).not.toHaveBeenCalled();
    await expect(store.jobByIdempotencyKey('live-repository-failure')).resolves.toBeUndefined();
  });

  it('treats an unavailable durable control row as closed', async () => {
    const store = {
      operatorControls: vi.fn(async () => {
        throw new Error('database unavailable');
      }),
    };
    await expect(
      assertOperatorControlOpen(store as unknown as MemoryStore, 'intake'),
    ).rejects.toBeInstanceOf(OperatorAdmissionError);
  });

  it('recovers an admitted settlement after the verifier App is removed', async () => {
    const store = new MemoryStore();
    const recoverableQuote = quoteWithAuthorization();
    await store.saveQuote(recoverableQuote);
    const binding = repositoryAdmissionBinding(
      recoverableQuote,
      'recovery-key',
      'signed-payment-proof',
    );
    const receipt = admissionReceipt(binding);
    const { job } = await store.createJob(
      recoverableQuote,
      {
        payer: 'payer',
        transaction: 'pending',
        amountAtomic: recoverableQuote.priceAtomic,
        signature: 'signed-payment-proof',
      },
      'recovery-key',
      receipt,
    );
    const order: string[] = [];
    const validateRepositoryAdmission = vi.fn(async () => {
      order.push('admission');
      return receipt;
    });
    const reconcileRepositorySettlement = vi.fn(async () => {
      order.push('reconciliation');
      throw new PolicyRequestError(
        'settlement_not_found',
        422,
        'finalized settlement was not found',
      );
    });
    const retrySettlement = vi.fn(async () => {
      order.push('settlement');
      return {
        payer: 'payer',
        transaction: 'settled-transaction',
        amountAtomic: recoverableQuote.priceAtomic,
        signature: 'signed-payment-proof',
      };
    });
    const registerRefundLiability = vi.fn(async () => ({
      ...refundLiability(recoverableQuote, job.id, 'settled-transaction'),
      id: '22222222-2222-4222-8222-222222222222',
    }));
    const createRepositoryAdmission = vi.fn(async () => {
      throw new Error('GitHub App installation was removed');
    });
    const base = await serve(
      dependencies(store, {
        config: livePaymentConfig,
        payments: { retrySettlement },
        policy: {
          validateRepositoryAdmission,
          reconcileRepositorySettlement,
          registerRefundLiability,
          createRepositoryAdmission,
        },
        processor: { process: vi.fn() },
      }),
    );
    const request = () =>
      fetch(`${base}/v1/admin/settlements/${job.id}`, {
        method: 'POST',
        headers: { authorization: 'Bearer admin-secret' },
      });

    const resumed = await request();
    expect(resumed.status).toBe(202);
    expect(retrySettlement).toHaveBeenCalledTimes(1);
    expect(validateRepositoryAdmission).toHaveBeenCalledWith(receipt, binding);
    expect(createRepositoryAdmission).not.toHaveBeenCalled();
    expect(order).toEqual(['admission', 'reconciliation', 'settlement']);
    await expect(store.operatorControls()).resolves.toMatchObject({ intakeEnabled: false });
    await expect(store.job(job.id)).resolves.toMatchObject({
      id: job.id,
      state: 'paid',
      payment: { transaction: 'settled-transaction' },
    });
  });
});

describe('public route responses', () => {
  it('rejects feature work before issuing a payment challenge', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'scope validation test intake',
      updatedBy: 'test',
    });
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => ({
            owner: 'example',
            repo: 'project',
            number: 2,
            title: 'Add a reset button',
            body: 'Expose a new UI control.',
            labels: ['enhancement'],
            defaultBranch: 'main',
            baseSha: 'a'.repeat(40),
            rootFiles: ['package.json', 'pnpm-lock.yaml'],
          })),
        },
        payments: { challenge },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/2' }),
    });

    expect(response.status).toBe(422);
    await expect(response.json()).resolves.toEqual({
      error: "issue labels place it outside Mizuki's maintenance-only scope",
    });
    expect(challenge).not.toHaveBeenCalled();
  });

  it('rejects issue drift before settlement', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    const settle = vi.fn();
    const assertIssueAuthorization = vi.fn(async () => {
      throw new Error('GitHub issue changed after the quote; request a new quote');
    });
    const base = await serve(
      dependencies(store, {
        github: { assertIssueAuthorization },
        payments: { settle },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'idempotency-key': 'drifted-job' },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({
      error: 'GitHub issue changed after the quote; request a new quote',
    });
    expect(assertIssueAuthorization).toHaveBeenCalledWith(
      quote.owner,
      quote.repo,
      quote.issueNumber,
      quote.installationId,
      quote.authorizationReceipt?.evidenceHash,
      { title: quote.issueTitle, body: quote.issueBody },
    );
    expect(settle).not.toHaveBeenCalled();
  });

  it('returns 429 with Retry-After when a source exceeds the OAuth bucket', async () => {
    const store = new MemoryStore();
    const authorizeUrl = vi.fn(() => 'https://github.com/login/oauth/authorize');
    const base = await serve(dependencies(store, { auth: { authorizeUrl } }));

    for (let index = 0; index < 10; index += 1) {
      const response = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
      expect(response.status).toBe(302);
    }
    const limited = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
    expect(limited.status).toBe(429);
    expect(limited.headers.get('retry-after')).toBe('6');
    expect(authorizeUrl).toHaveBeenCalledTimes(10);
  });

  it('sets a Secure OAuth session cookie from an authenticated HTTPS web request', async () => {
    const store = new MemoryStore();
    const proxySecret = 'p'.repeat(32);
    const base = await serve(
      dependencies(store, {
        config: {
          publicBaseUrl: 'https://mizuki-api.onrender.com',
          webOrigin: 'https://mizuki.covenant.org',
          trustedProxyHops: 1,
          webProxySecret: proxySecret,
        },
        auth: {
          callback: vi.fn(async () => ({ session: 'signed-session', redirect: '/bounties' })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/auth/github/callback?code=code&state=state`, {
      headers: {
        'x-forwarded-proto': 'http',
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': proxySecret,
      },
      redirect: 'manual',
    });

    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('https://mizuki.covenant.org/bounties');
    expect(response.headers.get('set-cookie')).toContain('; Secure');
  });

  it('closes an activity stream after its configured idle lifetime', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        config: { sseIdleTimeoutMs: 25 },
      }),
    );
    const startedAt = Date.now();
    const response = await fetch(`${base}/v1/events`);
    expect(response.status).toBe(200);
    await response.text();
    expect(Date.now() - startedAt).toBeLessThan(1_000);
  });
});

async function serve(deps: AppDependencies): Promise<string> {
  const server = createServer(createApp(deps));
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('test server did not bind');
  return `http://127.0.0.1:${address.port}`;
}

function dependencies(
  store: MemoryStore,
  overrides: Record<string, unknown> = {},
): AppDependencies {
  const { config: configOverride, ...dependencyOverrides } = overrides;
  return {
    config: {
      adminToken: 'admin-secret',
      paymentMode: 'mock',
      trustedProxyHops: 0,
      rateLimitMaxSources: 100,
      sseMaxConnections: 10,
      sseMaxConnectionsPerSource: 2,
      sseIdleTimeoutMs: 10_000,
      ...(configOverride as object | undefined),
    },
    store,
    github: {},
    payments: {},
    processor: {},
    auth: {},
    webhooks: {},
    bounties: {},
    policy: {},
    paymentAdmission: new SerialGate(),
    readiness: { check: vi.fn(async () => ({ ready: true })) },
    ...dependencyOverrides,
  } as unknown as AppDependencies;
}

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
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
};

const livePaymentConfig = {
  adminToken: 'admin-secret',
  paymentMode: 'live',
  payTo: 'refund-treasury',
  escrowRefundTo: 'escrow-authority',
  escrowReadinessMinLamports: 1_000_000,
};

const signerReadiness = {
  healthy: true,
  refundTreasury: 'refund-treasury',
  refundMint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
  refundDecimals: 6,
  finalizedBalanceRaw: '1000000000',
  pendingRefundRaw: '0',
  treasuryAvailableRefundRaw: '1000000000',
  remainingRefundLimitUsdCents: 100_000,
  availableRefundRaw: '1000000000',
  escrowAuthority: 'escrow-authority',
  finalizedEscrowBalanceLamports: '1000000000',
  availableEscrowReserveLamports: '1000000000',
};

const authorizedPayment = {
  payer: 'payer',
  transaction: 'pending',
  amountAtomic: quote.priceAtomic,
  signature: 'signed-payment-proof',
};

function admissionReceipt(
  binding: ReturnType<typeof repositoryAdmissionBinding>,
): RepositoryAdmissionReceipt {
  return {
    id: '33333333-3333-4333-8333-333333333333',
    ...binding,
    verifierAppId: '2',
    installationId: 1,
    repositorySelection: 'selected',
    permissions: {
      contents: 'read',
      issues: 'read',
      metadata: 'read',
      pull_requests: 'read',
    },
    tokenRepositories: 1,
    tokenExpiresAt: '2099-01-01T00:00:00.000Z',
    admittedAt: '2026-08-23T00:00:00.000Z',
    evidenceHash: 'f'.repeat(64),
  };
}

function quoteWithAuthorization(): Quote {
  return {
    ...quote,
    authorizationReceipt: {
      label: 'mizuki-approved',
      actorId: '1',
      actorLogin: 'maintainer',
      permission: 'admin',
      authorizedAt: '2026-08-22T00:00:00.000Z',
      verifiedAt: '2026-08-22T00:00:01.000Z',
      evidenceHash: 'e'.repeat(64),
    },
  };
}

function refundLiability(quoteValue: Quote, jobId: string, transaction: string) {
  return {
    jobId,
    repositoryAdmissionId: '33333333-3333-4333-8333-333333333333',
    settlementSignature: transaction,
    repository: `${quoteValue.owner}/${quoteValue.repo}`,
    issueNumber: quoteValue.issueNumber,
    baseRef: quoteValue.defaultBranch,
    baseSha: quoteValue.baseSha,
    repositoryAuthorizedAt: quoteValue.authorizationReceipt!.authorizedAt,
    authorizationEvidenceHash: quoteValue.authorizationReceipt!.evidenceHash,
    reviewedHeadSha: null,
    reviewedBaseSha: null,
    reviewedBaseRef: null,
    reviewedDiffHash: null,
    deliveryBoundAt: null,
    deliveryBindingHash: null,
    payer: 'payer',
    mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
    rawAmount: quoteValue.priceAtomic,
    decimals: 6,
    amountUsdCents: Number(quoteValue.priceAtomic) / 10_000,
    settlementSlot: 1,
    settlementBlockTimeUnixSeconds: 1,
    createdAt: '2026-08-23T00:00:00.000Z',
    dischargedAt: null,
    dischargeEvidenceHash: null,
  };
}
