import type { Config } from './config.js';
import {
  PolicyRequestError,
  refundLiabilityCommitment,
  repositoryAdmissionBinding,
  type PaymentPolicy,
  type RefundLiability,
  type SettlementEvidence,
} from './policy-client.js';
import type { MizukiStore } from './store.js';
import type { Job, Payment, RepositoryAdmissionReceipt } from './types.js';
import { Payments, USDC_DECIMALS, USDC_MAINNET } from './x402.js';

export interface SettlementRecoveryDependencies {
  paymentMode: Config['paymentMode'];
  payTo?: Config['payTo'];
  store: MizukiStore;
  payments: Pick<Payments, 'retrySettlement'>;
  policy: PaymentPolicy;
}

export async function recoverSettlement(
  job: Job,
  deps: SettlementRecoveryDependencies,
): Promise<Job> {
  if (job.state !== 'settlement_pending') {
    throw new Error('settlement is not pending');
  }

  let payment = job.payment;
  let liabilityId = job.refundLiabilityId;
  if (deps.paymentMode === 'live' && payment.transaction !== 'pending') {
    try {
      liabilityId = await registerLiability(job, payment, deps.policy);
    } catch (error) {
      if (!settlementSignatureRejected(error)) throw error;
      payment = await reconcileLivePayment(job, deps);
      await deps.store.patchJob(job.id, { payment });
      liabilityId = await registerLiability(job, payment, deps.policy);
    }
  } else {
    if (deps.paymentMode === 'live') {
      if (!payment.signature) throw new Error('stored payment signature is unavailable');
      if (!job.repositoryAdmission) {
        throw new Error('durable repository admission is unavailable');
      }
      await deps.policy.validateRepositoryAdmission(
        job.repositoryAdmission,
        repositoryAdmissionBinding(job.quote, job.idempotencyKey, payment.signature),
      );
      payment = await recoverLivePayment(job, deps);
    } else {
      payment = await deps.payments.retrySettlement(job.quote, payment);
    }
    await deps.store.patchJob(job.id, { payment });
    if (deps.paymentMode === 'live') {
      liabilityId = await registerLiability(job, payment, deps.policy);
    }
  }

  if (liabilityId && liabilityId !== job.refundLiabilityId) {
    await deps.store.patchJob(job.id, { refundLiabilityId: liabilityId });
  }
  return deps.store.transitionJob(job.id, 'settlement_pending', 'paid', {
    payment,
    refundLiabilityId: liabilityId,
  });
}

async function recoverLivePayment(
  job: Job,
  deps: SettlementRecoveryDependencies,
): Promise<Payment> {
  try {
    return await reconcileLivePayment(job, deps);
  } catch (error) {
    if (!settlementScanMiss(error)) throw error;
  }

  try {
    return await deps.payments.retrySettlement(job.quote, job.payment);
  } catch (facilitatorError) {
    try {
      return await reconcileLivePayment(job, deps);
    } catch (reconciliationError) {
      if (!settlementScanMiss(reconciliationError)) throw reconciliationError;
      throw facilitatorError;
    }
  }
}

async function reconcileLivePayment(
  job: Job,
  deps: SettlementRecoveryDependencies,
): Promise<Payment> {
  const evidence = await deps.policy.reconcileRepositorySettlement(job.repositoryAdmission!);
  return paymentFromEvidence(job, evidence, deps.payTo);
}

function paymentFromEvidence(
  job: Job,
  evidence: SettlementEvidence,
  payTo: string | undefined,
): Payment {
  if (!payTo) throw new Error('payment treasury is unavailable during settlement recovery');
  if (
    !evidence.finalized ||
    !evidence.succeeded ||
    evidence.payer !== job.payment.payer ||
    evidence.recipient !== payTo ||
    evidence.mint !== USDC_MAINNET ||
    evidence.decimals !== USDC_DECIMALS ||
    evidence.rawAmount !== job.quote.priceAtomic
  ) {
    throw new Error('reconciled settlement does not match the reserved payment');
  }
  return {
    ...job.payment,
    payer: evidence.payer,
    transaction: evidence.signature,
    amountAtomic: evidence.rawAmount,
  };
}

function settlementScanMiss(error: unknown): boolean {
  return (
    error instanceof PolicyRequestError &&
    ['settlement_not_found', 'settlement_scan_exhausted'].includes(error.code)
  );
}

function settlementSignatureRejected(error: unknown): boolean {
  return (
    error instanceof PolicyRequestError &&
    [
      'settlement_not_found',
      'payment_authorization_mismatch',
      'settlement_outside_payment_window',
    ].includes(error.code)
  );
}

export function assertLiabilityMatchesPayment(
  liability: RefundLiability,
  jobId: string,
  payment: Payment,
  quote: Job['quote'],
  admission: RepositoryAdmissionReceipt,
): void {
  const commitment = refundLiabilityCommitment(quote);
  if (
    liability.jobId !== jobId ||
    liability.settlementSignature !== payment.transaction ||
    liability.payer !== payment.payer ||
    liability.mint !== USDC_MAINNET ||
    liability.decimals !== USDC_DECIMALS ||
    liability.rawAmount !== payment.amountAtomic ||
    liability.amountUsdCents !== Number(payment.amountAtomic) / 10_000 ||
    liability.repositoryAdmissionId !== admission.id ||
    liability.repository !== commitment.repository ||
    liability.issueNumber !== commitment.issueNumber ||
    liability.baseRef !== commitment.baseRef ||
    liability.baseSha !== commitment.baseSha ||
    liability.repositoryAuthorizedAt !== commitment.repositoryAuthorizedAt ||
    liability.authorizationEvidenceHash !== commitment.authorizationEvidenceHash
  ) {
    throw new Error('refund liability evidence does not match the settled payment');
  }
}

async function registerLiability(
  job: Job,
  payment: Payment,
  policy: PaymentPolicy,
): Promise<string> {
  if (!job.repositoryAdmission) {
    throw new Error('durable repository admission is unavailable');
  }
  const liability = await policy.registerRefundLiability(
    job.id,
    payment.transaction,
    refundLiabilityCommitment(job.quote),
    job.repositoryAdmission,
  );
  assertLiabilityMatchesPayment(liability, job.id, payment, job.quote, job.repositoryAdmission);
  return liability.id;
}
