import Link from 'next/link';
import { formatUsd, formatUsdcAtomic, stateLabel } from '@/lib/format';
import type { Treasury } from '@/lib/types';

export function TreasurySnapshot({ treasury }: { treasury: Treasury }) {
  const protection = treasury.refundProtection;
  const max = Math.max(...treasury.allocationModel.buckets.map((bucket) => bucket.allocatedUsd), 1);
  const verified = protection.status === 'verified' && protection.finalizedBalanceAtomic !== null;

  return (
    <div className="treasury-snapshot">
      <div className="treasury-total">
        <span>{verified ? 'Verified refund reserve balance' : 'Refund protection status'}</span>
        <strong>
          {verified
            ? formatUsdcAtomic(protection.finalizedBalanceAtomic!)
            : stateLabel(protection.status)}
        </strong>
        <small>
          {protection.signerOutstandingLiabilityAtomic === null
            ? 'Finalized reserve records are unavailable'
            : protection.liabilityReconciled === true
              ? `${formatUsdcAtomic(protection.signerOutstandingLiabilityAtomic)} in outstanding refund obligations · records match`
              : 'Reserve records need reconciliation'}
        </small>
        <small>
          Service-record net flow: {formatUsd(treasury.recordedNetFlowUsd)}. Used for planning only;
          it is not a wallet balance.
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
        View reserve and transaction records <span aria-hidden="true">↗</span>
      </Link>
    </div>
  );
}
