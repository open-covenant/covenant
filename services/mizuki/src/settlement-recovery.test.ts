import { describe, expect, it, vi } from 'vitest';
import {
  PolicyRequestError,
  repositoryAdmissionBinding,
  type PaymentPolicy,
  type SettlementEvidence,
} from './policy-client.js';
import { recoverSettlement } from './settlement-recovery.js';
import { MemoryStore } from './store.js';
import type { Job, Quote, RepositoryAdmissionReceipt } from './types.js';

describe('settlement recovery', () => {
  it('never settles a Workbench payment before the signer reserves its payment intent', async () => {
    const store = new MemoryStore();
    const key = 'missing-payment-intent';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(
      quote,
      pendingPayment,
      key,
      receipt,
      '22222222-2222-4222-8222-222222222222',
    );
    const retrySettlement = vi.fn();
    const reserveError = new Error('refund capacity is unavailable');
    const reservePaymentIntent = vi.fn(async () => {
      throw reserveError;
    });

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: policy({ reservePaymentIntent }),
      }),
    ).rejects.toBe(reserveError);

    expect(reservePaymentIntent).toHaveBeenCalledWith(
      job.id,
      expect.objectContaining({ repository: 'example/project' }),
      receipt,
      1_000,
    );
    expect(retrySettlement).not.toHaveBeenCalled();
    const stored = await store.job(job.id);
    expect(stored).toMatchObject({
      state: 'settlement_pending',
      payment: { transaction: 'pending' },
    });
    expect(stored).not.toHaveProperty('paymentIntentId');
  });

  it('retries the exact signed payment when a protected intent has not reached the facilitator', async () => {
    const store = new MemoryStore();
    const key = 'protected-intent-before-facilitator';
    const receipt = admissionReceipt(key);
    const created = await store.createJob(
      quote,
      pendingPayment,
      key,
      receipt,
      '22222222-2222-4222-8222-222222222222',
    );
    const intentId = '55555555-5555-4555-8555-555555555555';
    const job = await store.patchJob(created.job.id, { paymentIntentId: intentId });
    const reconcilePaymentIntent = vi.fn(async () => {
      throw new PolicyRequestError(
        'payment_intent_pending',
        409,
        'payment intent is still inside its settlement window',
      );
    });
    const retrySettlement = vi.fn(async () => settledPayment);
    const activatePaymentIntent = vi.fn(async () => ({
      paymentIntent: {
        id: intentId,
        jobId: job.id,
        quoteId: quote.id,
        repositoryAdmissionId: receipt.id,
        status: 'activated',
        payer: settledPayment.payer,
        payee: PAY_TO,
        mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
        rawAmount: quote.priceAtomic,
        amountUsdCents: 200,
        bountyAmountUsdCents: 1_000,
        bountyReserveLamports: '100000000',
        memo: `mizuki:payment:v1:${quote.id}`,
        paymentWindowStartUnixSeconds: 1,
        paymentWindowEndUnixSeconds: 2,
        settlementSignature: settledPayment.transaction,
        liabilityId: LIABILITY_ID,
        createdAt: '2026-08-23T00:00:00.000Z',
        activatedAt: '2026-08-23T00:01:00.000Z',
        expiredAt: null,
      },
      refundLiability: liability(job.id),
    }));

    const recovered = await recoverSettlement(job, {
      paymentMode: 'live',
      payTo: PAY_TO,
      store,
      payments: { retrySettlement },
      policy: policy({
        reconcilePaymentIntent,
        validateRepositoryAdmission: vi.fn(async () => receipt),
        reconcileRepositorySettlement: vi.fn(async () => {
          throw settlementAbsent();
        }),
        activatePaymentIntent,
      }),
    });

    expect(recovered).toMatchObject({
      state: 'paid',
      payment: { transaction: settledPayment.transaction },
      paymentIntentId: intentId,
      refundLiabilityId: LIABILITY_ID,
    });
    expect(retrySettlement).toHaveBeenCalledOnce();
    expect(activatePaymentIntent).toHaveBeenCalledWith(intentId, settledPayment.transaction);
  });

  it('fails closed before an automatic retry when durable admission is missing', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(quote, pendingPayment, 'missing-admission');
    const retrySettlement = vi.fn();

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        store,
        payments: { retrySettlement },
        policy: policy({}),
      }),
    ).rejects.toThrow('durable repository admission is unavailable');

    expect(retrySettlement).not.toHaveBeenCalled();
    await expect(store.job(job.id)).resolves.toMatchObject({ state: 'settlement_pending' });
  });

  it('checkpoints settlement before liability registration and resumes without rebroadcast', async () => {
    const store = new MemoryStore();
    const key = 'crash-window-reservation';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const order: string[] = [];
    const validateRepositoryAdmission = vi.fn(async () => {
      order.push('admission');
      return receipt;
    });
    const reconcileRepositorySettlement = vi.fn(async () => {
      order.push('reconciliation');
      throw settlementAbsent();
    });
    const retrySettlement = vi.fn(async () => {
      order.push('settlement');
      return settledPayment;
    });
    const registerRefundLiability = vi.fn(async () => {
      order.push('liability');
      await expect(store.job(job.id)).resolves.toMatchObject({
        state: 'settlement_pending',
        payment: { transaction: settledPayment.transaction },
      });
      throw new Error('simulated crash after settlement checkpoint');
    });
    const paymentPolicy = policy({
      validateRepositoryAdmission,
      reconcileRepositorySettlement,
      registerRefundLiability,
    });

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: paymentPolicy,
      }),
    ).rejects.toThrow('simulated crash');
    expect(order).toEqual(['admission', 'reconciliation', 'settlement', 'liability']);

    registerRefundLiability.mockImplementation(async () => liability(job.id));
    validateRepositoryAdmission.mockRejectedValue(new Error('verifier App was removed'));
    const checkpointed = (await store.job(job.id))!;
    const recovered = await recoverSettlement(checkpointed, {
      paymentMode: 'live',
      payTo: PAY_TO,
      store,
      payments: { retrySettlement },
      policy: paymentPolicy,
    });

    expect(recovered).toMatchObject({
      state: 'paid',
      payment: { transaction: settledPayment.transaction },
      refundLiabilityId: LIABILITY_ID,
    });
    expect(retrySettlement).toHaveBeenCalledOnce();
    expect(validateRepositoryAdmission).toHaveBeenCalledOnce();
    expect(registerRefundLiability).toHaveBeenCalledTimes(2);
    expect(registerRefundLiability).toHaveBeenLastCalledWith(
      job.id,
      settledPayment.transaction,
      expect.objectContaining({ repository: 'example/project' }),
      receipt,
    );
  });

  it('fails closed when an already-finalized payment has no durable admission', async () => {
    const store = new MemoryStore();
    const { job } = await store.createJob(quote, settledPayment, 'already-finalized');
    const retrySettlement = vi.fn();
    const validateRepositoryAdmission = vi.fn();
    const registerRefundLiability = vi.fn(async () => liability(job.id));

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        store,
        payments: { retrySettlement },
        policy: policy({ validateRepositoryAdmission, registerRefundLiability }),
      }),
    ).rejects.toThrow('durable repository admission is unavailable');
    expect(retrySettlement).not.toHaveBeenCalled();
    expect(validateRepositoryAdmission).not.toHaveBeenCalled();
    expect(registerRefundLiability).not.toHaveBeenCalled();
  });

  it('registers an already-finalized payment from its durable admission without rebroadcast', async () => {
    const store = new MemoryStore();
    const key = 'already-finalized';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, settledPayment, key, receipt);
    const retrySettlement = vi.fn();
    const registerRefundLiability = vi.fn(async () => liability(job.id));

    const recovered = await recoverSettlement(job, {
      paymentMode: 'live',
      store,
      payments: { retrySettlement },
      policy: policy({ registerRefundLiability }),
    });

    expect(recovered.state).toBe('paid');
    expect(retrySettlement).not.toHaveBeenCalled();
    expect(registerRefundLiability).toHaveBeenCalledWith(
      job.id,
      settledPayment.transaction,
      expect.objectContaining({ repository: 'example/project' }),
      receipt,
    );
  });

  it.each([
    'settlement_not_found',
    'payment_authorization_mismatch',
    'settlement_outside_payment_window',
  ])('reconciles a canonical signature after registration rejects %s', async (code) => {
    const store = new MemoryStore();
    const key = `rejected-signature-${code}`;
    const receipt = admissionReceipt(key);
    const reportedPayment = { ...settledPayment, transaction: 'reported-settlement' };
    const { job } = await store.createJob(quote, reportedPayment, key, receipt);
    const retrySettlement = vi.fn();
    const reconcileRepositorySettlement = vi.fn(async () => settlementEvidence());
    const registerRefundLiability = vi
      .fn()
      .mockRejectedValueOnce(new PolicyRequestError(code, 422, 'reported signature rejected'))
      .mockImplementationOnce(async () => {
        await expect(store.job(job.id)).resolves.toMatchObject({
          payment: { transaction: settledPayment.transaction },
        });
        return liability(job.id);
      });

    const recovered = await recoverSettlement(job, {
      paymentMode: 'live',
      payTo: PAY_TO,
      store,
      payments: { retrySettlement },
      policy: policy({ reconcileRepositorySettlement, registerRefundLiability }),
    });

    expect(recovered).toMatchObject({
      state: 'paid',
      payment: { transaction: settledPayment.transaction },
      refundLiabilityId: LIABILITY_ID,
    });
    expect(registerRefundLiability).toHaveBeenNthCalledWith(
      1,
      job.id,
      reportedPayment.transaction,
      expect.any(Object),
      receipt,
    );
    expect(registerRefundLiability).toHaveBeenNthCalledWith(
      2,
      job.id,
      settledPayment.transaction,
      expect.any(Object),
      receipt,
    );
    expect(reconcileRepositorySettlement).toHaveBeenCalledOnce();
    expect(retrySettlement).not.toHaveBeenCalled();
  });

  it.each([
    ['rpc_unavailable', 503],
    ['settlement_scan_exhausted', 503],
    ['rpc_inconsistent', 503],
    ['refund_pool_insufficient', 503],
    ['refund_authorization_invalid', 401],
    ['settlement_value_mismatch', 422],
  ])('fails closed when known-signature registration returns %s', async (code, status) => {
    const store = new MemoryStore();
    const key = `registration-failure-${code}`;
    const receipt = admissionReceipt(key);
    const reportedPayment = { ...settledPayment, transaction: 'reported-settlement' };
    const { job } = await store.createJob(quote, reportedPayment, key, receipt);
    const error = new PolicyRequestError(code, status, 'registration failed closed');
    const retrySettlement = vi.fn();
    const reconcileRepositorySettlement = vi.fn();

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: policy({
          reconcileRepositorySettlement,
          registerRefundLiability: vi.fn(async () => {
            throw error;
          }),
        }),
      }),
    ).rejects.toBe(error);

    expect(reconcileRepositorySettlement).not.toHaveBeenCalled();
    expect(retrySettlement).not.toHaveBeenCalled();
    await expect(store.job(job.id)).resolves.toMatchObject({
      state: 'settlement_pending',
      payment: { transaction: reportedPayment.transaction },
    });
  });

  it('rejects liability evidence issued for a different repository admission', async () => {
    const store = new MemoryStore();
    const key = 'mismatched-liability-admission';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, settledPayment, key, receipt);

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        store,
        payments: { retrySettlement: vi.fn() },
        policy: policy({
          registerRefundLiability: vi.fn(async () => ({
            ...liability(job.id),
            repositoryAdmissionId: '55555555-5555-4555-8555-555555555555',
          })),
        }),
      }),
    ).rejects.toThrow('does not match the settled payment');

    await expect(store.job(job.id)).resolves.toMatchObject({ state: 'settlement_pending' });
  });

  it('does not broadcast when the signer rejects a tampered admission binding', async () => {
    const store = new MemoryStore();
    const key = 'tampered-reservation';
    const receipt = { ...admissionReceipt(key), paymentAuthorizationHash: '0'.repeat(64) };
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const retrySettlement = vi.fn();
    const validateRepositoryAdmission = vi.fn(async () => {
      throw new Error('repository admission does not match the settlement reservation');
    });

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        store,
        payments: { retrySettlement },
        policy: policy({ validateRepositoryAdmission }),
      }),
    ).rejects.toThrow('does not match');

    expect(validateRepositoryAdmission).toHaveBeenCalledWith(
      receipt,
      repositoryAdmissionBinding(quote, key, pendingPayment.signature!),
    );
    expect(retrySettlement).not.toHaveBeenCalled();
  });

  it('recovers a finalized settlement after the facilitator response is lost', async () => {
    const store = new MemoryStore();
    const key = 'lost-response-reservation';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const retrySettlement = vi.fn();
    const reconcileRepositorySettlement = vi.fn(async () => settlementEvidence());
    const registerRefundLiability = vi.fn(async () => liability(job.id));

    const recovered = await recoverSettlement(job, {
      paymentMode: 'live',
      payTo: PAY_TO,
      store,
      payments: { retrySettlement },
      policy: policy({
        validateRepositoryAdmission: vi.fn(async () => receipt),
        reconcileRepositorySettlement,
        registerRefundLiability,
      }),
    });

    expect(recovered).toMatchObject({
      state: 'paid',
      payment: { transaction: settledPayment.transaction },
      refundLiabilityId: LIABILITY_ID,
    });
    expect(reconcileRepositorySettlement).toHaveBeenCalledWith(receipt);
    expect(retrySettlement).not.toHaveBeenCalled();
  });

  it('retries the exact authorized transaction after the signer scan is exhausted', async () => {
    const store = new MemoryStore();
    const key = 'exhausted-scan-reservation';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const scanExhausted = new PolicyRequestError(
      'settlement_scan_exhausted',
      503,
      'settlement scan did not reach the authorization window',
    );
    const retrySettlement = vi.fn(async () => settledPayment);
    const reconcileRepositorySettlement = vi.fn(async () => {
      throw scanExhausted;
    });

    const recovered = await recoverSettlement(job, {
      paymentMode: 'live',
      payTo: PAY_TO,
      store,
      payments: { retrySettlement },
      policy: policy({
        validateRepositoryAdmission: vi.fn(async () => receipt),
        reconcileRepositorySettlement,
        registerRefundLiability: vi.fn(async () => liability(job.id)),
      }),
    });

    expect(recovered).toMatchObject({
      state: 'paid',
      payment: { transaction: settledPayment.transaction },
      refundLiabilityId: LIABILITY_ID,
    });
    expect(reconcileRepositorySettlement).toHaveBeenCalledOnce();
    expect(retrySettlement).toHaveBeenCalledOnce();
  });

  it.each(['duplicate_settlement', 'facilitator transport closed'])(
    'reconciles after a %s retry failure without requiring a facilitator signature',
    async (message) => {
      const store = new MemoryStore();
      const key = `retry-${message}`;
      const receipt = admissionReceipt(key);
      const { job } = await store.createJob(quote, pendingPayment, key, receipt);
      const reconcileRepositorySettlement = vi
        .fn()
        .mockRejectedValueOnce(settlementAbsent())
        .mockResolvedValueOnce(settlementEvidence());
      const retrySettlement = vi.fn(async () => {
        throw new Error(message);
      });

      const recovered = await recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: policy({
          validateRepositoryAdmission: vi.fn(async () => receipt),
          reconcileRepositorySettlement,
          registerRefundLiability: vi.fn(async () => liability(job.id)),
        }),
      });

      expect(recovered.state).toBe('paid');
      expect(retrySettlement).toHaveBeenCalledOnce();
      expect(reconcileRepositorySettlement).toHaveBeenCalledTimes(2);
    },
  );

  it('leaves the reservation pending when both RPCs report no settlement after retry failure', async () => {
    const store = new MemoryStore();
    const key = 'not-found-reservation';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const retrySettlement = vi.fn(async () => {
      throw new Error('duplicate_settlement');
    });
    const reconcileRepositorySettlement = vi.fn(async () => {
      throw settlementAbsent();
    });

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: policy({
          validateRepositoryAdmission: vi.fn(async () => receipt),
          reconcileRepositorySettlement,
        }),
      }),
    ).rejects.toThrow('duplicate_settlement');

    expect(reconcileRepositorySettlement).toHaveBeenCalledTimes(2);
    await expect(store.job(job.id)).resolves.toMatchObject({ state: 'settlement_pending' });
  });

  it('does not call the facilitator when independent settlement providers disagree', async () => {
    const store = new MemoryStore();
    const key = 'provider-disagreement';
    const receipt = admissionReceipt(key);
    const { job } = await store.createJob(quote, pendingPayment, key, receipt);
    const retrySettlement = vi.fn();
    const disagreement = new PolicyRequestError(
      'rpc_inconsistent',
      503,
      'independent RPC providers disagree',
    );

    await expect(
      recoverSettlement(job, {
        paymentMode: 'live',
        payTo: PAY_TO,
        store,
        payments: { retrySettlement },
        policy: policy({
          validateRepositoryAdmission: vi.fn(async () => receipt),
          reconcileRepositorySettlement: vi.fn(async () => {
            throw disagreement;
          }),
        }),
      }),
    ).rejects.toBe(disagreement);

    expect(retrySettlement).not.toHaveBeenCalled();
  });
});

const LIABILITY_ID = '44444444-4444-4444-8444-444444444444';
const ADMISSION_ID = '33333333-3333-4333-8333-333333333333';
const PAY_TO = 'treasury';

const quote: Quote = {
  id: '11111111-1111-4111-8111-111111111111',
  issueUrl: 'https://github.com/example/project/issues/17',
  owner: 'example',
  repo: 'project',
  issueNumber: 17,
  issueTitle: 'Fix failing test',
  issueBody: '',
  baseSha: 'a'.repeat(40),
  defaultBranch: 'main',
  authorizationReceipt: {
    label: 'mizuki-approved',
    actorId: '1',
    actorLogin: 'maintainer',
    permission: 'admin',
    authorizedAt: '2026-08-22T00:00:00.000Z',
    verifiedAt: '2026-08-22T00:00:01.000Z',
    evidenceHash: 'b'.repeat(64),
  },
  class: 'micro',
  priceAtomic: '2000000',
  maxFiles: 3,
  maxCostUsd: 0.8,
  validationCommands: [],
  expiresAt: '2099-01-01T00:00:00.000Z',
};

const pendingPayment = {
  payer: 'payer',
  transaction: 'pending',
  amountAtomic: quote.priceAtomic,
  signature: 'signed-payment-proof',
};

const settledPayment = {
  ...pendingPayment,
  transaction: 'settled-transaction',
};

function admissionReceipt(key: string): RepositoryAdmissionReceipt {
  return {
    id: ADMISSION_ID,
    ...repositoryAdmissionBinding(quote, key, pendingPayment.signature),
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
    tokenExpiresAt: '2026-08-23T01:00:00.000Z',
    admittedAt: '2026-08-23T00:00:00.000Z',
    evidenceHash: 'c'.repeat(64),
  };
}

function liability(jobId: string) {
  return {
    id: LIABILITY_ID,
    jobId,
    repositoryAdmissionId: ADMISSION_ID,
    settlementSignature: settledPayment.transaction,
    repository: 'example/project',
    issueNumber: quote.issueNumber,
    baseRef: quote.defaultBranch,
    baseSha: quote.baseSha,
    repositoryAuthorizedAt: quote.authorizationReceipt!.authorizedAt,
    authorizationEvidenceHash: quote.authorizationReceipt!.evidenceHash,
    reviewedHeadSha: null,
    reviewedBaseSha: null,
    reviewedBaseRef: null,
    reviewedDiffHash: null,
    deliveryBoundAt: null,
    deliveryBindingHash: null,
    payer: settledPayment.payer,
    mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
    rawAmount: quote.priceAtomic,
    decimals: 6,
    amountUsdCents: 200,
    settlementSlot: 1,
    settlementBlockTimeUnixSeconds: 1,
    createdAt: '2026-08-23T00:00:00.000Z',
    dischargedAt: null,
    dischargeEvidenceHash: null,
  };
}

function settlementEvidence(): SettlementEvidence {
  return {
    signature: settledPayment.transaction,
    payer: settledPayment.payer,
    recipient: PAY_TO,
    mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
    rawAmount: quote.priceAtomic,
    decimals: 6,
    finalized: true,
    succeeded: true,
    slot: 1,
    blockTimeUnixSeconds: 1,
  };
}

function settlementAbsent(): PolicyRequestError {
  return new PolicyRequestError('settlement_not_found', 422, 'finalized settlement was not found');
}

function policy(overrides: Record<string, unknown>): PaymentPolicy {
  return overrides as unknown as PaymentPolicy;
}
