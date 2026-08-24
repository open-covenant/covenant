import Link from 'next/link';
import { bountyStateLabel, failureLabel, formatUsd, relativeTime } from '@/lib/format';
import type { Bounty } from '@/lib/types';
import { StatusBadge } from './status-badge';

export function BountyCard({ bounty }: { bounty: Bounty }) {
  return (
    <article className="bounty-card">
      <div className="bounty-card-topline">
        <StatusBadge state={bounty.state} label={bountyStateLabel(bounty.state)} />
        <span className="bounty-value">
          {bounty.asset === 'SOL' ? 'Approx. ' : ''}
          {formatUsd(bounty.amountUsd)}
        </span>
      </div>
      <div>
        <p className="eyebrow">{bounty.repository}</p>
        <h3>{bounty.title}</h3>
      </div>
      <div className="bounty-meta">
        <span>{failureLabel(bounty.failureClass)}</span>
        <span>{relativeTime(bounty.updatedAt)}</span>
      </div>
      <Link href={`/bounties/${encodeURIComponent(bounty.id)}`} className="card-link">
        View bounty <span aria-hidden="true">↗</span>
      </Link>
    </article>
  );
}
