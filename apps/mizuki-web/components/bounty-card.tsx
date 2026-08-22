import Link from 'next/link';
import { formatUsd, relativeTime } from '@/lib/format';
import type { Bounty } from '@/lib/types';
import { StatusBadge } from './status-badge';

export function BountyCard({ bounty }: { bounty: Bounty }) {
  return (
    <article className="bounty-card">
      <div className="bounty-card-topline">
        <StatusBadge state={bounty.state} />
        <span className="bounty-value">{formatUsd(bounty.amountUsd)}</span>
      </div>
      <div>
        <p className="eyebrow">{bounty.repository}</p>
        <h3>{bounty.title}</h3>
      </div>
      <div className="bounty-meta">
        <span>{bounty.failureClass?.replaceAll('_', ' ') || 'maintenance rescue'}</span>
        <span>{relativeTime(bounty.updatedAt)}</span>
      </div>
      <Link href={`/bounties/${encodeURIComponent(bounty.id)}`} className="card-link">
        Inspect bounty <span aria-hidden="true">↗</span>
      </Link>
    </article>
  );
}
