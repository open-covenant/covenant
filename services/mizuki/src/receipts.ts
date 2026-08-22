import type { MizukiStore } from './store.js';
import type { Job } from './types.js';

export async function recordPaymentReceipts(store: MizukiStore, job: Job): Promise<void> {
  if (job.state === 'settlement_pending' || job.payment.transaction === 'pending') return;
  await store.appendLedger({
    kind: 'customer_payment',
    referenceId: job.id,
    asset: 'USDC',
    amountAtomic: job.payment.amountAtomic,
    amountUsd: Number(job.payment.amountAtomic) / 1_000_000,
    transaction: job.payment.transaction,
  });
  const exists = (await store.activity(500)).some(
    (event) => event.kind === 'job.paid' && event.subjectId === job.id,
  );
  if (!exists) {
    await store.appendActivity('job.paid', job.id, {
      issueUrl: job.quote.issueUrl,
      class: job.quote.class,
      settlementTransaction: job.payment.transaction,
    });
  }
}
