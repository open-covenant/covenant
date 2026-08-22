import type { Metadata } from 'next';
import { DataError, DemoNotice } from '@/components/data-state';
import { TransactionLink } from '@/components/transaction-link';
import { getTreasury } from '@/lib/api';
import { formatSolLamports, formatTime, formatUsd, formatUsdcAtomic } from '@/lib/format';
import type { LedgerEntry } from '@/lib/types';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Public accounting',
  description:
    'Inspect signer-verified refund custody, exact liabilities, recorded ledger flows, modeled allocations, and transaction receipts.',
};

export default async function TreasuryPage() {
  const result = await getTreasury();
  if (result.status === 'error') {
    return (
      <div className="page-shell shell fatal-state">
        <DataError title="Treasury unavailable" detail={result.error} />
      </div>
    );
  }
  const treasury = result.data;
  const protection = treasury.refundProtection;
  const verified = protection.status === 'verified' && protection.finalizedBalanceAtomic !== null;
  return (
    <div className="page-shell">
      <section className="treasury-hero shell">
        <div>
          <p className="eyebrow">Custody proof + public ledger {result.demo && <DemoNotice />}</p>
          <h1>Custody and allocations are different evidence.</h1>
          <p>
            Finalized policy-signer evidence is the only source of truth for refund custody. The USD
            waterfall below is an application-ledger allocation model, not a wallet balance or spend
            authorization. Rescue bounties use separate signer-controlled SOL escrow.
          </p>
        </div>
        <div className="treasury-hero-total">
          <span>{verified ? 'Signer-verified refund custody' : 'Refund protection status'}</span>
          <strong>
            {verified ? formatUsdcAtomic(protection.finalizedBalanceAtomic!) : protection.status}
          </strong>
          <small>
            {protection.checkedAt
              ? `Signer checked ${formatTime(protection.checkedAt)}`
              : 'Fresh signer evidence unavailable'}
          </small>
        </div>
      </section>

      <section className="shell treasury-page-grid">
        <div>
          <div className="waterfall-heading">
            <p className="eyebrow">Application-ledger allocation model</p>
            <span>
              {formatUsd(treasury.localOutstandingLiabilityUsd)} locally recorded liabilities
            </span>
          </div>
          <ol className="waterfall">
            {treasury.allocationModel.buckets.map((bucket, index) => {
              const target = bucket.targetUsd;
              const ratio = target ? Math.min(100, (bucket.allocatedUsd / target) * 100) : 100;
              return (
                <li key={bucket.id}>
                  <div className="waterfall-index">{String(index + 1).padStart(2, '0')}</div>
                  <div className="waterfall-body">
                    <div className="waterfall-value">
                      <strong>{bucket.label}</strong>
                      <span>{formatUsd(bucket.allocatedUsd)}</span>
                    </div>
                    <div
                      className="waterfall-track"
                      aria-label={
                        target
                          ? `${Math.round(ratio)} percent of modeled target`
                          : 'Planned allocation'
                      }
                    >
                      <span style={{ width: `${ratio}%` }} />
                    </div>
                    <small>
                      {target
                        ? `${formatUsd(target)} modeled target`
                        : 'Planned allocation; not custody or spend authority'}
                    </small>
                  </div>
                </li>
              );
            })}
          </ol>
          <div className="policy-note">
            <strong>Signer evidence boundary</strong>
            <p>
              Status: {protection.status}. Signer liabilities{' '}
              {protection.signerOutstandingLiabilityAtomic === null
                ? 'are unavailable'
                : `are ${formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)}`}
              . Local and signer liabilities{' '}
              {protection.liabilityReconciled === true
                ? 'reconcile'
                : protection.liabilityReconciled === false
                  ? 'do not reconcile'
                  : 'cannot be compared without fresh evidence'}
              .
            </p>
          </div>
        </div>

        <div className="ledger-panel">
          <div className="ledger-heading">
            <div>
              <p className="eyebrow">Ledger</p>
              <h2>Recent movement</h2>
            </div>
            <span>
              {treasury.plannedRunwayDays === null
                ? 'No modeled cost baseline'
                : `${treasury.plannedRunwayDays} modeled days`}
            </span>
          </div>
          {treasury.ledger.length > 0 ? (
            <ol className="ledger-list">
              {treasury.ledger.map((entry) => (
                <li key={entry.id}>
                  <div className={`ledger-amount ledger-${entry.direction}`}>
                    {entry.direction === 'credit' ? '+' : entry.direction === 'debit' ? '−' : '→'}
                    {formatLedgerAmount(entry)}
                  </div>
                  <div>
                    <strong>{entry.description}</strong>
                    <span>
                      {entry.type.replaceAll('_', ' ')} · {formatTime(entry.occurredAt)}
                    </span>
                  </div>
                  {entry.transaction && (
                    <TransactionLink signature={entry.transaction} label="Receipt" />
                  )}
                </li>
              ))}
            </ol>
          ) : (
            <p className="receipt-empty">No ledger entries have been published.</p>
          )}
        </div>
      </section>
    </div>
  );
}

function formatLedgerAmount(entry: LedgerEntry): string {
  if (entry.asset === 'SOL' && entry.amountAtomic !== undefined) {
    return formatSolLamports(entry.amountAtomic);
  }
  if (entry.amountUsd !== undefined) return formatUsd(entry.amountUsd);
  if (entry.amountAtomic !== undefined && entry.asset) {
    return `${entry.amountAtomic} ${entry.asset}`;
  }
  return 'Unpriced';
}
