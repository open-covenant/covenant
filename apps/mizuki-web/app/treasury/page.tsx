import type { Metadata } from 'next';
import { DataError, DemoNotice } from '@/components/data-state';
import { TransactionLink } from '@/components/transaction-link';
import { getTreasury } from '@/lib/api';
import {
  formatSolLamports,
  formatTime,
  formatUsd,
  formatUsdcAtomic,
  stateLabel,
  truncateAddress,
} from '@/lib/format';
import { pageMetadata } from '@/lib/page-metadata';
import type { LedgerEntry } from '@/lib/types';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = pageMetadata({
  title: 'Refund reserve and financial records',
  description:
    'View refund reserve coverage, customer refund obligations, recorded transactions, and allocation plans.',
  path: '/treasury',
});

export default async function TreasuryPage() {
  const result = await getTreasury();
  if (result.status === 'error') {
    return (
      <div className="page-shell shell fatal-state">
        <DataError title="Refund and financial records are temporarily unavailable" />
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
          <p className="eyebrow">
            Refund reserve and transaction history {result.demo && <DemoNotice />}
          </p>
          <h1>Verified reserve funds are separate from planning estimates.</h1>
          <p>
            The refund panel comes from the separate signer&apos;s finalized on-chain balance and
            registered refund obligations. The planning panel is calculated from service records; it
            is not a wallet balance and cannot authorize a transfer. Bounty payouts use a different
            SOL escrow.
          </p>
        </div>
        <div className="treasury-hero-total">
          <span>
            {verified ? 'Verified refund reserve balance' : 'Refund protection needs attention'}
          </span>
          <strong>
            {verified
              ? formatUsdcAtomic(protection.finalizedBalanceAtomic!)
              : stateLabel(protection.status)}
          </strong>
          <small>
            {protection.checkedAt
              ? `Verified ${formatTime(protection.checkedAt)}`
              : 'Current signer evidence is unavailable'}
          </small>
        </div>
      </section>

      <section className="shell treasury-page-grid">
        <div>
          <div className="waterfall-heading">
            <p className="eyebrow">Operating plan · not a wallet balance</p>
            <span>
              {formatUsd(treasury.localOutstandingLiabilityUsd)} in refund obligations recorded by
              the service
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
                          ? `${Math.round(ratio)} percent of planned target`
                          : 'Planned allocation'
                      }
                    >
                      <span style={{ width: `${ratio}%` }} />
                    </div>
                    <small>
                      {target
                        ? `${formatUsd(target)} planned target`
                        : 'Planning estimate; not reserve funds or spending authority'}
                    </small>
                  </div>
                </li>
              );
            })}
          </ol>
          <div className="policy-note">
            <strong>Reserve reconciliation</strong>
            <p>
              Reserve status: {stateLabel(protection.status)}. The separate signer reports{' '}
              {protection.signerOutstandingLiabilityAtomic === null
                ? 'an unavailable amount'
                : formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)}{' '}
              in outstanding refund obligations. These records{' '}
              {protection.liabilityReconciled === true
                ? 'match the service job records'
                : protection.liabilityReconciled === false
                  ? 'do not match the service job records'
                  : 'cannot currently be compared with the service job records'}
              .
            </p>
          </div>
          <div className="policy-note reserve-details">
            <strong>Refund reserve details</strong>
            <dl className="receipt-list">
              <div>
                <dt>Treasury address</dt>
                <dd>{reserveAccount(protection.refundTreasury)}</dd>
              </div>
              <div>
                <dt>Finalized balance</dt>
                <dd>
                  {protection.finalizedBalanceAtomic === null
                    ? 'Unavailable'
                    : formatUsdcAtomic(protection.finalizedBalanceAtomic)}
                </dd>
              </div>
              <div>
                <dt>Outstanding refund obligations</dt>
                <dd>
                  {protection.signerOutstandingLiabilityAtomic === null
                    ? 'Unavailable'
                    : formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)}
                </dd>
              </div>
              <div>
                <dt>Balance after refund obligations</dt>
                <dd>
                  {protection.unencumberedBalanceAtomic === null
                    ? 'Unavailable'
                    : formatUsdcAtomic(protection.unencumberedBalanceAtomic)}
                </dd>
              </div>
              <div>
                <dt>New-job capacity</dt>
                <dd>
                  {protection.newIntakeCapacityAtomic === null
                    ? 'Unavailable'
                    : formatUsdcAtomic(protection.newIntakeCapacityAtomic)}
                </dd>
              </div>
              <div>
                <dt>Daily refund authorization remaining</dt>
                <dd>
                  {protection.remainingDailyLimitUsdCents === null
                    ? 'Unavailable'
                    : formatUsd(protection.remainingDailyLimitUsdCents / 100)}
                </dd>
              </div>
            </dl>
          </div>
        </div>

        <div className="ledger-panel">
          <div className="ledger-heading">
            <div>
              <p className="eyebrow">Financial records</p>
              <h2>Recent transactions and estimates</h2>
            </div>
            <span>
              {treasury.plannedRunwayDays === null
                ? 'Runway estimate unavailable'
                : `Estimated runway: ${treasury.plannedRunwayDays} days`}
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
                      {ledgerEvidenceLabel(entry)} · {formatTime(entry.occurredAt)}
                    </span>
                  </div>
                  {entry.transaction && (
                    <TransactionLink signature={entry.transaction} label="View transaction" />
                  )}
                </li>
              ))}
            </ol>
          ) : (
            <p className="receipt-empty">No financial records have been published.</p>
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
  return 'Amount unavailable';
}

function ledgerEvidenceLabel(entry: LedgerEntry): string {
  if (entry.transaction) return 'On-chain transaction';
  if (entry.type === 'allocation' || entry.type === 'treasury_deposit') {
    return 'Planning allocation';
  }
  if (entry.type === 'refund_obligation') return 'Outstanding refund obligation';
  if (entry.type === 'route_cost') return 'Recorded cost estimate';
  if (entry.type === 'operating_cost') return 'Recorded operating cost';
  return 'Service financial record';
}

function reserveAccount(address: string | null) {
  if (!address) return 'Unavailable';
  const base = process.env.NEXT_PUBLIC_SOLANA_EXPLORER_URL || 'https://solscan.io';
  const cluster =
    process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? '?cluster=devnet' : '';
  return (
    <a
      href={`${base.replace(/\/$/, '')}/account/${encodeURIComponent(address)}${cluster}`}
      target="_blank"
      rel="noreferrer"
    >
      {truncateAddress(address, 7)} <span aria-hidden="true">↗</span>
    </a>
  );
}
