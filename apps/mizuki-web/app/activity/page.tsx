import type { Metadata } from 'next';
import { ActivityFeed } from '@/components/activity-feed';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { getActivity } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Public activity',
  description:
    'Follow Mizuki’s paid jobs, refunds, bounties, pull requests, payouts, upgrades, and rollbacks.',
};

export default async function ActivityPage() {
  const result = await getActivity();
  return (
    <div className="page-shell">
      <section className="page-hero shell activity-page-hero">
        <div>
          <p className="eyebrow">
            Public event stream {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Follow every material outcome.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            The feed records actions, not promises: paid work, finalized refunds, secured bounties,
            accepted patches, payouts, activations, and rollbacks.
          </p>
          <div className="event-key">
            <span>
              <i className="key-paid" /> Money in
            </span>
            <span>
              <i className="key-refund" /> Refund
            </span>
            <span>
              <i className="key-work" /> Work
            </span>
            <span>
              <i className="key-upgrade" /> Capability
            </span>
          </div>
        </div>
      </section>
      <section className="shell activity-page-section">
        {result.status === 'error' ? (
          <DataError title="Activity stream unavailable" detail={result.error} />
        ) : result.status === 'empty' ? (
          <EmptyState title="No material events yet">
            The first paid job will begin the public record.
          </EmptyState>
        ) : (
          <ActivityFeed initial={result.data} live={!result.demo} />
        )}
      </section>
    </div>
  );
}
