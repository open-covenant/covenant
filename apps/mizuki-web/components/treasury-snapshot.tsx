import Link from 'next/link';
import { formatUsd, formatUsdcAtomic } from '@/lib/format';
import type { Treasury } from '@/lib/types';

export function TreasurySnapshot({ treasury }: { treasury: Treasury }) {
  const protection = treasury.refundProtection;
  const max = Math.max(...treasury.allocationModel.buckets.map((bucket) => bucket.allocatedUsd), 1);
  const verified = protection.status === 'verified' && protection.finalizedBalanceAtomic !== null;

  return (
    <div className="treasury-snapshot">
      <div className="treasury-total">
        <span>{verified ? 'Signer-verified refund custody' : 'Refund protection'}</span>
        <strong>
          {verified ? formatUsdcAtomic(protection.finalizedBalanceAtomic!) : protection.status}
        </strong>
        <small>
          {protection.signerOutstandingLiabilityAtomic === null
            ? 'Fresh finalized signer evidence is unavailable'
            : `${formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)} signer liabilities · ${protection.liabilityReconciled ? 'reconciled' : 'mismatch'}`}
        </small>
        <small>
          Recorded application-ledger net flow: {formatUsd(treasury.recordedNetFlowUsd)}. It is not
          a wallet balance.
        </small>
      </div>
      <div className="treasury-bars">
        {treasury.allocationModel.buckets.map((bucket) => (
          <div className="treasury-bar-row" key={bucket.id}>
            <div>
              <span>{bucket.label}</span>
              <strong>{formatUsd(bucket.allocatedUsd)}</strong>
            </div>
            <span className="treasury-bar-track" aria-hidden="true">
              <span style={{ width: `${Math.max(4, (bucket.allocatedUsd / max) * 100)}%` }} />
            </span>
          </div>
        ))}
      </div>
      <Link href="/treasury" className="text-link">
        Inspect custody and allocation evidence <span aria-hidden="true">↗</span>
      </Link>
    </div>
  );
}
