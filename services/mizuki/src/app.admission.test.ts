import { createServer, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  assertOperatorControlOpen,
  createApp,
  OperatorAdmissionError,
  SerialGate,
  type AppDependencies,
} from './app.js';
import { GithubOAuthCallbackError } from './auth.js';
import { GithubReadinessError } from './github.js';
import { PolicyRequestError, repositoryAdmissionBinding } from './policy-client.js';
import { GithubOAuthCapacityError, MemoryStore } from './store.js';
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
  vi.restoreAllMocks();
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
    expect(initial.headers.get('cache-control')).toBe('no-store');
    await expect(initial.json()).resolves.toMatchObject({
      intakeEnabled: false,
      claimsEnabled: false,
      revision: 0,
    });

    const body = JSON.stringify({
      expectedRevision: 0,
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

  it('requires a revision, rejects stale reopens, and exposes the authenticated audit', async () => {
    const store = new MemoryStore();
    const base = await serve(dependencies(store));
    const headers = {
      authorization: 'Bearer admin-secret',
      'content-type': 'application/json',
    };

    const missingRevision = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        intakeEnabled: true,
        reason: 'opening without a bound revision',
      }),
    });
    expect(missingRevision.status).toBe(400);
    await expect(missingRevision.json()).resolves.toEqual({
      error: 'expectedRevision must be an integer between 0 and 2147483647',
    });

    const oversizedRevision = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        expectedRevision: 2_147_483_648,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'reject revisions outside the database range',
      }),
    });
    expect(oversizedRevision.status).toBe(400);
    await expect(oversizedRevision.json()).resolves.toEqual({
      error: 'expectedRevision must be an integer between 0 and 2147483647',
    });

    const opened = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        expectedRevision: 0,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'open one bounded canary window',
      }),
    });
    expect(opened.status).toBe(200);

    const closed = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'emergency closure wins over stale state',
      }),
    });
    expect(closed.status).toBe(200);
    await expect(closed.json()).resolves.toMatchObject({
      revision: 2,
      intakeEnabled: false,
      claimsEnabled: false,
    });

    const staleOpen = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        expectedRevision: 0,
        intakeEnabled: true,
        claimsEnabled: true,
        reason: 'delayed retry from an earlier open request',
      }),
    });
    expect(staleOpen.status).toBe(409);
    await expect(staleOpen.json()).resolves.toEqual({
      error: 'expected operator admission revision 0; current revision is 2',
    });

    const futureClose = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        expectedRevision: 100,
        intakeEnabled: false,
        claimsEnabled: false,
        reason: 'reject a closure from an impossible future revision',
      }),
    });
    expect(futureClose.status).toBe(409);
    await expect(futureClose.json()).resolves.toEqual({
      error: 'expected operator admission revision 100; current revision is 2',
    });

    const unauthorizedAudit = await fetch(`${base}/v1/admin/admission/audit`);
    expect(unauthorizedAudit.status).toBe(401);
    expect(unauthorizedAudit.headers.get('cache-control')).toBe('private, no-store');
    const audit = await fetch(`${base}/v1/admin/admission/audit`, { headers });
    expect(audit.status).toBe(200);
    await expect(audit.json()).resolves.toEqual([
      expect.objectContaining({
        revision: 0,
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: false,
      }),
      expect.objectContaining({
        revision: 1,
        expectedRevision: 0,
        intakeEnabled: true,
        claimsEnabled: true,
      }),
      expect.objectContaining({
        revision: 2,
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: false,
      }),
    ]);
    await expect(store.operatorControls()).resolves.toMatchObject({
      revision: 2,
      intakeEnabled: false,
      claimsEnabled: false,
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
        expectedRevision: 0,
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

  it('closes intake while preserving open claims during a readiness outage', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: true,
      reason: 'prepare an open state for emergency closure',
      updatedBy: 'operator',
    });
    const base = await serve(
      dependencies(store, {
        readiness: { check: vi.fn(async () => ({ ready: false })) },
      }),
    );

    const response = await fetch(`${base}/v1/admin/admission`, {
      method: 'POST',
      headers: { authorization: 'Bearer admin-secret', 'content-type': 'application/json' },
      body: JSON.stringify({
        expectedRevision: 0,
        intakeEnabled: false,
        claimsEnabled: true,
        reason: 'close paid intake while contributor claims continue',
      }),
    });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      revision: 2,
      intakeEnabled: false,
      claimsEnabled: true,
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
        expectedRevision: 0,
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
      expectedRevision: 0,
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
      expectedRevision: 0,
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

  it('rejects a payment before settlement when its rescue bounty exceeds rolling capacity', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'rescue capacity test',
      updatedBy: 'test',
    });
    const settle = vi.fn();
    const createRepositoryAdmission = vi.fn();
    const base = await serve(
      dependencies(store, {
        config: livePaymentConfig,
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        policy: {
          readiness: vi.fn(async () => ({
            ...signerReadiness,
            remainingEscrowLimitUsdCents: 999,
          })),
          createRepositoryAdmission,
        },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': 'rescue-capacity-job',
        'payment-signature': 'signed-payment-proof',
      },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({
      error: 'rescue-bounty protection is temporarily unavailable',
    });
    expect(settle).not.toHaveBeenCalled();
    expect(createRepositoryAdmission).not.toHaveBeenCalled();
    await expect(store.jobByIdempotencyKey('rescue-capacity-job')).resolves.toBeUndefined();
  });

  it('holds rescue capacity after refund until the bounty has signer-backed funding', async () => {
    const store = new MemoryStore();
    const existingQuote: Quote = {
      ...quote,
      id: '22222222-2222-4222-8222-222222222222',
      issueUrl: 'https://github.com/example/project/issues/2',
      issueNumber: 2,
    };
    await store.saveQuote(existingQuote);
    const { job: refundedJob } = await store.createJob(
      existingQuote,
      { payer: 'payer', transaction: 'existing-payment', amountAtomic: '2000000' },
      'existing-paid-job',
    );
    await store.transitionJob(refundedJob.id, 'settlement_pending', 'refunded', {
      refundTransaction: 'existing-refund',
    });
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'contingent rescue capacity test',
      updatedBy: 'test',
    });
    const settle = vi.fn();
    const base = await serve(
      dependencies(store, {
        config: livePaymentConfig,
        github: {
          assertIssueAuthorization: vi.fn(async () => undefined),
          currentHead: vi.fn(async () => quote.baseSha),
        },
        payments: { settle },
        policy: {
          readiness: vi.fn(async () => ({
            ...signerReadiness,
            remainingEscrowLimitUsdCents: 1_999,
          })),
        },
      }),
    );

    const response = await fetch(`${base}/v1/jobs`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'idempotency-key': 'second-paid-job',
        'payment-signature': 'signed-payment-proof',
      },
      body: JSON.stringify({ quote_id: quote.id }),
    });

    expect(response.status).toBe(503);
    expect(settle).not.toHaveBeenCalled();
    await expect(store.jobByIdempotencyKey('second-paid-job')).resolves.toBeUndefined();
  });

  it('probes the exact verifier repository inside the live gate before settlement', async () => {
    const store = new MemoryStore();
    await store.saveQuote(quote);
    await store.updateOperatorControls({
      expectedRevision: 0,
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
      expectedRevision: 0,
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
  it('rejects pull requests as job intake with a clear client error', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'pull request input test',
      updatedBy: 'test',
    });
    const challenge = vi.fn();
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => {
            throw new Error(
              'Choose an open GitHub issue for paid maintenance. Existing pull requests cannot be used as job intake.',
            );
          }),
        },
        payments: { challenge },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/7' }),
    });

    expect(response.status).toBe(422);
    await expect(response.json()).resolves.toEqual({
      error:
        'Choose an open GitHub issue for paid maintenance. Existing pull requests cannot be used as job intake.',
    });
    expect(challenge).not.toHaveBeenCalled();
  });

  it('returns a correlated 503 without exposing GitHub failure details', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      expectedRevision: 0,
      intakeEnabled: true,
      claimsEnabled: false,
      reason: 'GitHub readiness response test',
      updatedBy: 'test',
    });
    const log = vi.spyOn(console, 'error').mockImplementation(() => {});
    const base = await serve(
      dependencies(store, {
        github: {
          issue: vi.fn(async () => {
            throw new GithubReadinessError(
              'unavailable',
              'GitHub repository access is temporarily unavailable. Please try again shortly.',
              403,
            );
          }),
        },
      }),
    );

    const response = await fetch(`${base}/v1/quotes`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', authorization: 'Bearer private-value' },
      body: JSON.stringify({ github_issue_url: 'https://github.com/example/project/issues/2' }),
    });

    expect(response.status).toBe(503);
    expect(response.headers.get('x-request-id')).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    await expect(response.json()).resolves.toEqual({
      error: 'GitHub repository access is temporarily unavailable. Please try again shortly.',
    });
    expect(log).toHaveBeenCalledOnce();
    const entry = JSON.parse(String(log.mock.calls[0]?.[0])) as Record<string, unknown>;
    expect(entry).toMatchObject({
      level: 'error',
      event: 'http_request_failed',
      requestId: response.headers.get('x-request-id'),
      method: 'POST',
      path: '/v1/quotes',
      status: 503,
      error: { type: 'GithubReadinessError', code: 'unavailable', upstreamStatus: 403 },
    });
    expect(JSON.stringify(entry)).not.toContain('private-value');
    expect(JSON.stringify(entry)).not.toContain('example/project');
  });

  it('rejects feature work before issuing a payment challenge', async () => {
    const store = new MemoryStore();
    await store.updateOperatorControls({
      expectedRevision: 0,
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
    const beginGithubOAuth = vi.fn(async () => ({
      url: 'https://github.com/login/oauth/authorize',
      flowCookie: 'browser-flow',
    }));
    const base = await serve(dependencies(store, { auth: { beginGithubOAuth } }));

    for (let index = 0; index < 10; index += 1) {
      const response = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
      expect(response.status).toBe(302);
    }
    const limited = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });
    expect(limited.status).toBe(429);
    expect(limited.headers.get('retry-after')).toBe('6');
    expect(beginGithubOAuth).toHaveBeenCalledTimes(10);
  });

  it('sets a short-lived browser-bound OAuth cookie at authorization start', async () => {
    const store = new MemoryStore();
    const proxySecret = 'p'.repeat(32);
    const beginGithubOAuth = vi.fn(async () => ({
      url: 'https://github.com/login/oauth/authorize?state=signed-state',
      flowCookie: 'browser-flow-secret',
    }));
    const base = await serve(
      dependencies(store, {
        config: { trustedProxyHops: 1, webProxySecret: proxySecret },
        auth: { beginGithubOAuth },
      }),
    );

    const response = await fetch(`${base}/v1/auth/github?return_to=%2Fapp`, {
      headers: {
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': proxySecret,
      },
      redirect: 'manual',
    });

    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toContain('state=signed-state');
    expect(response.headers.get('set-cookie')).toBe(
      'mizuki_oauth_flow=browser-flow-secret; Path=/api/mizuki/v1/auth/github/callback; HttpOnly; SameSite=Lax; Max-Age=600; Secure',
    );
    expect(beginGithubOAuth).toHaveBeenCalledWith('/app');
  });

  it('binds pull request authorization to the OAuth flow', async () => {
    const store = new MemoryStore();
    const beginGithubOAuth = vi.fn(async () => ({
      url: 'https://github.com/login/oauth/authorize?state=signed-state',
      flowCookie: 'browser-flow-secret',
    }));
    const base = await serve(dependencies(store, { auth: { beginGithubOAuth } }));

    const response = await fetch(
      `${base}/v1/auth/github?return_to=%2Fapp%2Fjobs%2Fnew&authorize_pr=https%3A%2F%2Fgithub.com%2Fopen-covenant%2Fcovenant%2Fpull%2F196`,
      { redirect: 'manual' },
    );

    expect(response.status).toBe(302);
    expect(beginGithubOAuth).toHaveBeenCalledWith(
      '/app/jobs/new',
      'https://github.com/open-covenant/covenant/pull/196',
    );
  });

  it('binds issue authorization to the OAuth flow', async () => {
    const store = new MemoryStore();
    const beginGithubOAuth = vi.fn(async () => ({
      url: 'https://github.com/login/oauth/authorize?state=signed-state',
      flowCookie: 'browser-flow-secret',
    }));
    const base = await serve(dependencies(store, { auth: { beginGithubOAuth } }));

    const response = await fetch(
      `${base}/v1/auth/github?return_to=%2Fapp%2Frepositories%2Fopen-covenant%2Fcovenant&authorize_issue=https%3A%2F%2Fgithub.com%2Fopen-covenant%2Fcovenant%2Fissues%2F197`,
      { redirect: 'manual' },
    );

    expect(response.status).toBe(302);
    expect(beginGithubOAuth).toHaveBeenCalledWith(
      '/app/repositories/open-covenant/covenant',
      undefined,
      'https://github.com/open-covenant/covenant/issues/197',
    );
  });

  it('fails closed with retry guidance when browser OAuth capacity is exhausted', async () => {
    const store = new MemoryStore();
    const base = await serve(
      dependencies(store, {
        auth: {
          beginGithubOAuth: vi.fn(async () => {
            throw new GithubOAuthCapacityError();
          }),
        },
      }),
    );

    const response = await fetch(`${base}/v1/auth/github`, { redirect: 'manual' });

    expect(response.status).toBe(503);
    expect(response.headers.get('retry-after')).toBe('60');
    expect(response.headers.get('cache-control')).toBe('private, no-store');
    await expect(response.json()).resolves.toEqual({
      error: 'GitHub sign-in is temporarily busy; try again shortly',
    });
  });

  it('sets a Secure OAuth session cookie from an authenticated HTTPS web request', async () => {
    const store = new MemoryStore();
    const proxySecret = 'p'.repeat(32);
    const callback = vi.fn(async () => ({ session: 'signed-session', redirect: '/bounties' }));
    const base = await serve(
      dependencies(store, {
        config: {
          publicBaseUrl: 'https://mizuki-api.onrender.com',
          webOrigin: 'https://mizuki.opencovenant.org',
          trustedProxyHops: 1,
          webProxySecret: proxySecret,
        },
        auth: { callback },
      }),
    );

    const response = await fetch(`${base}/v1/auth/github/callback?code=code&state=state`, {
      headers: {
        cookie: 'mizuki_oauth_flow=browser-flow-secret',
        'x-forwarded-proto': 'http',
        'x-mizuki-forwarded-proto': 'https',
        'x-mizuki-proxy-secret': proxySecret,
      },
      redirect: 'manual',
    });

    expect(response.status).toBe(302);
    expect(response.headers.get('location')).toBe('https://mizuki.opencovenant.org/bounties');
    expect(response.headers.get('set-cookie')).toContain('mizuki_oauth_flow=;');
    expect(response.headers.get('set-cookie')).toContain('mizuki_session=signed-session;');
    expect(response.headers.get('set-cookie')).toContain('; Secure');
    expect(callback).toHaveBeenCalledWith('code', 'state', 'browser-flow-secret');
  });

  it('clears the OAuth flow cookie and preserves verified callback return paths', async () => {
    const store = new MemoryStore();
    const callback = vi
      .fn()
      .mockRejectedValueOnce(new GithubOAuthCallbackError('replayed'))
      .mockRejectedValueOnce(new GithubOAuthCallbackError('expired'))
      .mockRejectedValueOnce(new GithubOAuthCallbackError('inactive'))
      .mockRejectedValueOnce(new Error('private database detail'));
    const githubOAuthRedirect = vi.fn((state: string | undefined) =>
      state === 'signed-state' ? '/bounties/bounty-7?source=claim' : undefined,
    );
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const base = await serve(
      dependencies(store, {
        config: {
          publicBaseUrl: 'https://mizuki-api.onrender.com',
          webOrigin: 'https://mizuki.opencovenant.org',
        },
        auth: { callback, githubOAuthRedirect },
      }),
    );

    const incomplete = await fetch(`${base}/v1/auth/github/callback?state=signed-state`, {
      headers: { cookie: 'mizuki_oauth_flow=sensitive-browser-secret' },
      redirect: 'manual',
    });
    expect(incomplete.status).toBe(302);
    expect(incomplete.headers.get('location')).toBe(
      'https://mizuki.opencovenant.org/bounties/bounty-7?source=claim&auth_error=incomplete',
    );
    expect(incomplete.headers.get('set-cookie')).toContain('mizuki_oauth_flow=;');
    expect(incomplete.headers.get('set-cookie')).toContain('Max-Age=0');

    const denied = await fetch(
      `${base}/v1/auth/github/callback?error=access_denied&state=signed-state`,
      {
        headers: { cookie: 'mizuki_oauth_flow=sensitive-browser-secret' },
        redirect: 'manual',
      },
    );
    expect(denied.headers.get('location')).toBe(
      'https://mizuki.opencovenant.org/bounties/bounty-7?source=claim&auth_error=denied',
    );

    const invalidState = await fetch(
      `${base}/v1/auth/github/callback?error=access_denied&state=invalid-state`,
      { redirect: 'manual' },
    );
    expect(invalidState.headers.get('location')).toBe(
      'https://mizuki.opencovenant.org/app?auth_error=denied',
    );

    for (const expected of ['replayed', 'expired', 'inactive', 'unavailable']) {
      const rejected = await fetch(
        `${base}/v1/auth/github/callback?code=temporary-code&state=signed-state`,
        {
          headers: { cookie: 'mizuki_oauth_flow=sensitive-browser-secret' },
          redirect: 'manual',
        },
      );
      expect(rejected.status).toBe(302);
      expect(rejected.headers.get('location')).toBe(
        `https://mizuki.opencovenant.org/bounties/bounty-7?source=claim&auth_error=${expected}`,
      );
      expect(rejected.headers.get('set-cookie')).toContain('mizuki_oauth_flow=;');
      expect(await rejected.text()).not.toContain('sensitive-browser-secret');
      expect(rejected.headers.get('location')).not.toContain('private database detail');
    }
    expect(callback).toHaveBeenCalledTimes(4);
    expect(githubOAuthRedirect).toHaveBeenCalledWith('signed-state');
    expect(githubOAuthRedirect).toHaveBeenCalledWith('invalid-state');
    expect(JSON.stringify(consoleError.mock.calls)).not.toContain('private database detail');
    expect(JSON.stringify(consoleError.mock.calls)).not.toContain('sensitive-browser-secret');
    consoleError.mockRestore();
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
  remainingEscrowLimitUsdCents: 100_000,
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
