'use client';

import Link from 'next/link';
import { formatTime, formatUsdcAtomic, truncateAddress } from '@/lib/format';
import { normalizeBilling, type BillingEntry } from '@/lib/workbench';
import { useWorkbenchResource } from '@/lib/workbench-client';
import {
  SummaryCard,
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

export function Billing() {
  const billing = useWorkbenchResource('/v1/account/billing', normalizeBilling);

  if (billing.status === 'loading') {
    return (
      <div className="workbench-page">
        <WorkbenchLoading label="Loading payments and refunds" />
      </div>
    );
  }
  if (billing.status === 'error') {
    return (
      <div className="workbench-page">
        <WorkbenchError
          title="Payment records could not be loaded"
          detail="Existing payments and refunds continue independently."
          retry={billing.refresh}
        />
      </div>
    );
  }
  if (billing.status !== 'ready') return null;

  const payments = billing.data.entries.filter((entry) => entry.kind === 'payment');
  const refunds = billing.data.entries.filter((entry) => entry.kind === 'refund');
  const confirmingPayments = payments.filter((entry) => entry.state === 'pending');
  const pendingRefunds = refunds.filter((entry) => entry.state === 'pending');
  const paidAtomic = payments
    .filter((entry) => entry.state === 'finalized')
    .reduce((total, entry) => total + BigInt(entry.amountAtomic || '0'), 0n)
    .toString();
  const refundedAtomic = refunds
    .filter((entry) => entry.state === 'finalized')
    .reduce((total, entry) => total + BigInt(entry.amountAtomic || '0'), 0n)
    .toString();

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Account records"
        title="Payments & refunds"
        description="Inspect direct USDC job payments and refunds returned to the original payer."
        action={<Link href="/app/jobs/new">New maintenance job</Link>}
      />

      <div className="billing-policy-notice">
        <div>
          <span>Payment method</span>
          <strong>Pay per job with a Solana wallet</strong>
          <p>Mizuki does not hold a prepaid customer balance in Workbench.</p>
        </div>
        {billing.data.walletAddress && (
          <div>
            <span>Most recent payer wallet</span>
            <strong>{truncateAddress(billing.data.walletAddress, 7)}</strong>
            <p>Refund destinations are bound to each original payment.</p>
          </div>
        )}
      </div>

      <div className="workbench-summary-grid billing-summary-grid">
        <SummaryCard
          label="Payments confirming"
          value={confirmingPayments.length}
          detail="Awaiting settlement confirmation; do not pay again"
        />
        <SummaryCard
          label="Paid"
          value={formatUsdcAtomic(paidAtomic)}
          detail="Finalized direct job payments"
        />
        <SummaryCard
          label="Refunded"
          value={formatUsdcAtomic(refundedAtomic)}
          detail="Finalized returns to payer wallets"
        />
        <SummaryCard
          label="Refunds pending"
          value={pendingRefunds.length}
          detail="Protected payments awaiting finalization"
        />
      </div>

      {billing.data.truncated && (
        <p className="billing-scope-note">
          Every payment or refund still in progress is included. Completed history is limited to the
          latest {billing.data.limit?.toLocaleString() ?? 'available'} jobs, so totals cover the
          records shown rather than the account’s complete lifetime.
        </p>
      )}

      <section className="workbench-panel">
        <div className="workbench-panel-heading">
          <div>
            <span>USDC activity</span>
            <h2>Transaction history</h2>
          </div>
          <button type="button" onClick={billing.refresh}>
            Refresh
          </button>
        </div>
        {billing.data.entries.length > 0 ? (
          <div className="billing-ledger">
            <div className="billing-ledger-head" aria-hidden="true">
              <span>Type</span>
              <span>Job</span>
              <span>Status</span>
              <span>Amount</span>
              <span>Recorded</span>
              <span />
            </div>
            {billing.data.entries.map((entry) => (
              <BillingEntryRow entry={entry} key={entry.id} />
            ))}
          </div>
        ) : (
          <WorkbenchEmpty
            title="No payments or refunds yet"
            detail="Direct job payments and any refund transactions will appear here."
            action={<Link href="/app/jobs/new">Start a job</Link>}
          />
        )}
      </section>
    </div>
  );
}

export function BillingEntryRow({ entry }: { entry: BillingEntry }) {
  return (
    <article aria-label={`${entry.kind === 'refund' ? 'Refund' : 'Payment'} record`}>
      <div className="billing-ledger-cell">
        <span className="billing-ledger-label">Type</span>
        <strong>{entry.kind === 'refund' ? 'Refund' : 'Payment'}</strong>
      </div>
      <div className="billing-ledger-cell">
        <span className="billing-ledger-label">Job</span>
        <span>
          {entry.jobId ? (
            <Link href={`/app/jobs/${encodeURIComponent(entry.jobId)}`}>
              {entry.repository || entry.jobId.slice(0, 10)}
            </Link>
          ) : (
            entry.repository || 'Job record'
          )}
        </span>
      </div>
      <div className="billing-ledger-cell">
        <span className="billing-ledger-label">Status</span>
        <WorkbenchStatus value={entry.state} />
      </div>
      <div className="billing-ledger-cell">
        <span className="billing-ledger-label">Amount</span>
        <span>{formatUsdcAtomic(entry.amountAtomic)}</span>
      </div>
      <div className="billing-ledger-cell billing-ledger-recorded">
        <span className="billing-ledger-label">Recorded</span>
        <span>{entry.occurredAt ? formatTime(entry.occurredAt) : 'Time unavailable'}</span>
      </div>
      <div className="billing-ledger-cell billing-ledger-evidence">
        <span className="billing-ledger-label">Evidence</span>
        {entry.transaction ? (
          <a
            href={`https://solscan.io/tx/${encodeURIComponent(entry.transaction)}`}
            target="_blank"
            rel="noreferrer"
            aria-label={`Open ${entry.kind} transaction evidence on Solscan`}
          >
            Transaction ↗
          </a>
        ) : (
          <span>Not finalized</span>
        )}
      </div>
    </article>
  );
}
