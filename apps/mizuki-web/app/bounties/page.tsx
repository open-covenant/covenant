import type { Metadata } from 'next';
import { BountyBoard } from '@/components/bounty-board';
import { DataError, DemoNotice, EmptyState } from '@/components/data-state';
import { getBounties } from '@/lib/api';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Maintenance bounties',
  description:
    'Claim maintenance bounties published only after the original customer refund and separate SOL escrow have finalized.',
};

export default async function BountiesPage() {
  const result = await getBounties();
  return (
    <div className="page-shell">
      <section className="page-hero shell">
        <div>
          <p className="eyebrow">
            Funded maintenance bounties {result.status !== 'error' && result.demo && <DemoNotice />}
          </p>
          <h1>Finish a refunded issue and earn the escrowed SOL payout.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Every listed bounty is published only after the original payer&apos;s USDC refund and
            the bounty&apos;s separate SOL escrow have finalized. The repository maintainer retains
            review and merge control.
          </p>
          <ol className="mini-process">
            <li>
              <span>1</span> Sign in with GitHub and verify your payout wallet
            </li>
            <li>
              <span>2</span> Claim the fixed 48-hour work period
            </li>
            <li>
              <span>3</span> Open a scoped pull request and pass repository checks and a separate AI
              review
            </li>
            <li>
              <span>4</span> Obtain approval from a non-claimant maintainer on the exact reviewed
              commit, then merge before the deadline
            </li>
            <li>
              <span>5</span> Receive the escrowed SOL payout after the policy signer verifies every
              requirement
            </li>
          </ol>
          <p className="bounty-terms-note">
            Claiming does not guarantee payout. Every listed requirement must pass and the pull
            request must merge before the claim expires. The displayed USD amount is the reference
            value used to calculate the fixed SOL escrow.
          </p>
        </div>
      </section>
      <section className="shell board-section">
        {result.status === 'error' ? (
          <DataError title="Bounty records are temporarily unavailable" />
        ) : result.status === 'empty' ? (
          <EmptyState title="No funded maintenance bounties are open">
            A bounty appears only after the original customer refund and the bounty&apos;s separate
            SOL escrow have both finalized.
          </EmptyState>
        ) : (
          <BountyBoard bounties={result.data} />
        )}
      </section>
    </div>
  );
}
