'use client';

import Link from 'next/link';
import { useMemo, useState } from 'react';
import { BountyActions } from '@/components/bounty-actions';
import { GithubClaimButton } from '@/components/github-claim-button';
import { TransactionLink } from '@/components/transaction-link';
import { WalletProof } from '@/components/wallet-proof';
import { bountyStateLabel, failureLabel, formatTime } from '@/lib/format';
import { bountyPayoutText, normalizeBounties, normalizeBounty } from '@/lib/workbench';
import { useWorkbenchResource } from '@/lib/workbench-client';
import {
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

type BountyFilter = 'available' | 'my_work' | 'completed';

export function BountyWorkspace() {
  const availableBounties = useWorkbenchResource('/v1/bounties', normalizeBounties);
  const accountBounties = useWorkbenchResource('/v1/account/bounties', normalizeBounties);
  const [filter, setFilter] = useState<BountyFilter>('available');
  const visible = useMemo(() => {
    if (filter === 'available') {
      return availableBounties.status === 'ready'
        ? availableBounties.data.filter((item) => item.state === 'open')
        : [];
    }
    if (accountBounties.status !== 'ready') return [];
    if (filter === 'completed') {
      return accountBounties.data.filter((item) =>
        ['accepted', 'released', 'expired', 'rejected', 'refunded'].includes(item.state),
      );
    }
    return accountBounties.data.filter((item) =>
      ['claimed', 'pr_submitted', 'validating', 'disputed'].includes(item.state),
    );
  }, [accountBounties, availableBounties, filter]);
  const currentResource = filter === 'available' ? availableBounties : accountBounties;

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Funded rescue work"
        title="Bounties"
        description="Claim funded maintenance work, submit the exact pull request, and track review and payout evidence."
        action={<Link href="/bounties">View public board</Link>}
      />
      <div className="workbench-filter-bar">
        <div aria-label="Filter bounties">
          {(['available', 'my_work', 'completed'] as const).map((value) => (
            <button
              type="button"
              className={filter === value ? 'active' : ''}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
              key={value}
            >
              {value.replace('_', ' ')}
            </button>
          ))}
        </div>
        {currentResource.status === 'ready' && <span>{visible.length} bounties</span>}
      </div>
      {currentResource.status === 'loading' ? (
        <WorkbenchLoading label="Loading bounty work" />
      ) : currentResource.status === 'error' ? (
        <WorkbenchError
          title="Bounty work could not be loaded"
          detail="Existing claims and secured payouts remain unchanged."
          retry={currentResource.refresh}
        />
      ) : visible.length > 0 ? (
        <div className="workbench-bounty-grid">
          {visible.map((bounty) => (
            <article className="workbench-bounty-card" key={bounty.id}>
              <div>
                <WorkbenchStatus value={bounty.state} />
                <BountyValue amountAtomic={bounty.amountAtomic} amountUsd={bounty.amountUsd} />
              </div>
              <span>{bounty.repository}</span>
              <h2>{bounty.title}</h2>
              <p>{failureLabel(bounty.failureClass)}</p>
              {bounty.claimExpiresAt && (
                <small>Lease ends {formatTime(bounty.claimExpiresAt)}</small>
              )}
              <Link href={`/app/bounties/${encodeURIComponent(bounty.id)}`}>
                Open workspace <span aria-hidden="true">↗</span>
              </Link>
            </article>
          ))}
        </div>
      ) : currentResource.status === 'ready' ? (
        <WorkbenchEmpty
          title={
            filter === 'available'
              ? 'No funded bounties are available'
              : `No ${filter.replace('_', ' ')} bounties`
          }
          detail="Change the filter or inspect the public board for the complete bounty record."
          action={<Link href="/bounties">View public board</Link>}
        />
      ) : null}
    </div>
  );
}

export function BountyRoom({ id }: { id: string }) {
  const bounty = useWorkbenchResource(`/v1/bounties/${encodeURIComponent(id)}`, normalizeBounty);

  if (bounty.status === 'loading') {
    return (
      <div className="workbench-page">
        <WorkbenchLoading label="Loading bounty workspace" />
      </div>
    );
  }
  if (bounty.status === 'error') {
    return (
      <div className="workbench-page">
        <WorkbenchError
          title="This bounty could not be loaded"
          detail="The claim and escrow state were not changed."
          retry={bounty.refresh}
        />
      </div>
    );
  }
  if (bounty.status !== 'ready') return null;
  const value = bounty.data;
  const claimable =
    value.state === 'open' && Boolean(value.escrowTransaction && value.amountAtomic);
  const returnTo = `/app/bounties/${encodeURIComponent(value.id)}`;
  const payout = bountyPayoutText(value.amountAtomic, value.amountUsd);

  return (
    <div className="workbench-page workbench-bounty-room">
      <header className="bounty-room-header">
        <div>
          <p>{value.repository}</p>
          <h1>{value.title}</h1>
          <span>{bountyStateLabel(value.state)}</span>
        </div>
        <div>
          <span>{value.escrowTransaction ? 'Secured payout' : 'Payout not secured'}</span>
          <strong>{payout.exact}</strong>
          <small>{payout.approximate} at publication</small>
        </div>
      </header>

      <div className="bounty-room-grid">
        <div className="bounty-room-main">
          <section className="workbench-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>Acceptance contract</span>
                <h2>Required work</h2>
              </div>
              <WorkbenchStatus value={value.state} />
            </div>
            <ol className="workbench-criteria-list">
              {value.acceptanceCriteria.map((criterion, index) => (
                <li key={criterion}>
                  <span>{String(index + 1).padStart(2, '0')}</span>
                  <p>{criterion}</p>
                </li>
              ))}
            </ol>
            <a href={value.issueUrl} target="_blank" rel="noreferrer">
              Inspect source issue ↗
            </a>
          </section>

          <section className="workbench-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>Protected transformation</span>
                <h2>Refund before rescue work</h2>
              </div>
            </div>
            <div className="bounty-evidence-chain">
              <EvidenceStep
                number="01"
                title="Paid attempt stopped"
                detail={failureLabel(value.failureClass)}
              />
              <EvidenceStep
                number="02"
                title="Customer made whole"
                detail="The rescue is separate from the original paid contract."
                transaction={value.customerRefundTransaction}
                transactionLabel="Refund"
              />
              <EvidenceStep
                number="03"
                title={value.escrowTransaction ? 'Payout secured' : 'Funding not finalized'}
                detail="The reward cannot be reused while this bounty is active."
                transaction={value.escrowTransaction}
                transactionLabel="Escrow"
              />
            </div>
          </section>
        </div>

        <aside className="workbench-panel bounty-claim-panel">
          <span>Contributor workspace</span>
          <h2>{claimable ? 'Claim this work' : bountyStateLabel(value.state)}</h2>
          {claimable ? (
            <>
              <div className="bounty-claim-step">
                <span>01</span>
                <div>
                  <strong>Verify GitHub identity</strong>
                  <p>The claim is tied to the contributor who opens the pull request.</p>
                </div>
              </div>
              <GithubClaimButton bountyId={value.id} returnTo={returnTo} />
              <WalletProof bountyId={value.id} onMutated={bounty.refresh} />
            </>
          ) : value.claimant ? (
            <BountyActions
              bountyId={value.id}
              state={value.state}
              claimantLogin={value.claimant.github}
              pullRequestUrl={value.pullRequestUrl}
              hasDispute={Boolean(value.dispute)}
              returnTo={returnTo}
              onMutated={bounty.refresh}
            />
          ) : (
            <p>This bounty is not accepting new claims.</p>
          )}
          <dl className="bounty-claim-rules">
            <div>
              <dt>Work lease</dt>
              <dd>48 hours</dd>
            </div>
            <div>
              <dt>Review</dt>
              <dd>Separate AI review</dd>
            </div>
            <div>
              <dt>Updated</dt>
              <dd>{formatTime(value.updatedAt)}</dd>
            </div>
          </dl>
        </aside>
      </div>
    </div>
  );
}

function BountyValue({ amountAtomic, amountUsd }: { amountAtomic?: string; amountUsd: number }) {
  const payout = bountyPayoutText(amountAtomic, amountUsd);

  return (
    <div className="workbench-bounty-value">
      <strong>{payout.exact}</strong>
      <small>{payout.approximate}</small>
    </div>
  );
}

function EvidenceStep({
  number,
  title,
  detail,
  transaction,
  transactionLabel,
}: {
  number: string;
  title: string;
  detail: string;
  transaction?: string;
  transactionLabel?: string;
}) {
  return (
    <div>
      <span>{number}</span>
      <strong>{title}</strong>
      <p>{detail}</p>
      {transaction && transactionLabel && (
        <TransactionLink signature={transaction} label={transactionLabel} />
      )}
    </div>
  );
}
