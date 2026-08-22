import type { Metadata } from 'next';
import { BountyBoard } from '@/components/bounty-board';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { getBounties } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Rescue bounties',
  description: 'Claim public maintenance bounties created from fully refunded Mizuki jobs.',
};

export default async function BountiesPage() {
  const result = await getBounties();
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">
            Failure-to-capability market{' '}
            {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Get paid to finish what Mizuki could not.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Each rescue begins after the customer has been refunded. Scope, funds, acceptance, and
            payout evidence stay public.
          </p>
          <ol className="mini-process">
            <li>
              <span>1</span> Sign in and prove a payout wallet
            </li>
            <li>
              <span>2</span> Hold one immutable 48-hour work window
            </li>
            <li>
              <span>3</span> Pass checks, review, and acceptance
            </li>
            <li>
              <span>4</span> Receive the escrowed payout
            </li>
          </ol>
        </div>
      </section>
      <section className="shell board-section">
        {result.status === 'error' ? (
          <DataError title="Bounty board unavailable" detail={result.error} />
        ) : result.status === 'empty' ? (
          <EmptyState title="No rescue work is open">
            A bounty appears only after a paid attempt fails, its customer is refunded, and funds
            are secured.
          </EmptyState>
        ) : (
          <BountyBoard bounties={result.data} />
        )}
      </section>
    </div>
  );
}
