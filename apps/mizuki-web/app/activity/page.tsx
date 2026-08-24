import type { Metadata } from 'next';
import { ActivityFeed } from '@/components/activity-feed';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { getActivity } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Public activity',
  description:
    'Follow Mizuki’s published payments, pull requests, refunds, funded bounties, payouts, production changes, and rollbacks.',
};

export default async function ActivityPage() {
  const result = await getActivity();
  return (
    <div className="page-shell">
      <section className="page-hero shell activity-page-hero">
        <div>
          <p className="eyebrow">
            Public service activity {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Follow each material service outcome.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            The record distinguishes service events from finalized on-chain transactions. Payments,
            pull-request delivery, refunds, bounty funding, payouts, production changes, and
            rollbacks are published as they are recorded.
          </p>
          <div className="event-key">
            <span>
              <i className="key-paid" /> Payment
            </span>
            <span>
              <i className="key-refund" /> Refunds
            </span>
            <span>
              <i className="key-work" /> Delivery
            </span>
            <span>
              <i className="key-upgrade" /> Production change
            </span>
          </div>
        </div>
      </section>
      <section className="shell activity-page-section">
        {result.status === 'error' ? (
          <DataError title="The activity log is temporarily unavailable" />
        ) : result.status === 'empty' ? (
          <EmptyState title="No activity has been published yet">
            The first paid job will begin the public record.
          </EmptyState>
        ) : (
          <ActivityFeed initial={result.data} live={!result.demo} />
        )}
      </section>
    </div>
  );
}
