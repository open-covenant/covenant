'use client';

import Link from 'next/link';
import { formatTime, formatUsdcAtomic, truncateAddress } from '@/lib/format';
import { normalizeBilling } from '@/lib/workbench';
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
  const pendingRefunds = refunds.filter((entry) => entry.state === 'pending');
  const paidAtomic = payments
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
          label="Paid"
          value={formatUsdcAtomic(paidAtomic)}
          detail="Direct job payments"
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
          Showing the latest {billing.data.limit?.toLocaleString() ?? 'available'} jobs. The totals
          on this page cover the records shown, not the account’s complete history.
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
              <article key={entry.id}>
                <strong>{entry.kind === 'refund' ? 'Refund' : 'Payment'}</strong>
                <span>
                  {entry.jobId ? (
                    <Link href={`/app/jobs/${encodeURIComponent(entry.jobId)}`}>
                      {entry.repository || entry.jobId.slice(0, 10)}
                    </Link>
                  ) : (
                    entry.repository || 'Job record'
                  )}
                </span>
                <WorkbenchStatus value={entry.state} />
                <span>{formatUsdcAtomic(entry.amountAtomic)}</span>
                <span>{entry.occurredAt ? formatTime(entry.occurredAt) : 'Time unavailable'}</span>
                <span>
                  {entry.transaction ? (
                    <a
                      href={`https://solscan.io/tx/${encodeURIComponent(entry.transaction)}`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      Transaction ↗
                    </a>
                  ) : (
                    '—'
                  )}
                </span>
              </article>
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
