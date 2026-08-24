import type { Metadata } from 'next';
import { notFound } from 'next/navigation';
import { BountyActions } from '@/components/bounty-actions';
import { DataError, DemoNotice } from '@/components/data-state';
import { GithubClaimButton } from '@/components/github-claim-button';
import { ProviderReceiptDetails } from '@/components/provider-receipt';
import { StatusBadge } from '@/components/status-badge';
import { TransactionLink } from '@/components/transaction-link';
import { WalletProof } from '@/components/wallet-proof';
import { getBounty } from '@/lib/api';
import { formatTime, formatUsd, stateLabel } from '@/lib/format';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = {
  title: 'Bounty receipt',
  description:
    'Inspect scope, refund transformation, claim, validation, and payout evidence for a Mizuki rescue bounty.',
};

export default async function BountyDetailPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  const result = await getBounty(id);
  if (result.status === 'not_found') notFound();
  if (result.status === 'error') {
    return (
      <div className="page-shell shell fatal-state">
        <DataError title="Bounty unavailable" detail={result.error} />
      </div>
    );
  }
  const bounty = result.data;
  const claimable = bounty.state === 'open';
  return (
    <div className="page-shell">
      <section className="bounty-detail-hero shell">
        <div className="bounty-detail-topline">
          <div className="eyebrow">
            {bounty.repository} · issue #{bounty.issueNumber ?? '—'} {result.demo && <DemoNotice />}
          </div>
          <StatusBadge state={bounty.state} />
        </div>
        <div className="bounty-detail-title">
          <h1>{bounty.title}</h1>
          <div className="detail-price">
            <span>{bounty.escrowTransaction ? 'Secured payout' : 'Proposed payout'}</span>
            <strong>{formatUsd(bounty.amountUsd)}</strong>
            <small>{bounty.asset || 'USDC'}</small>
          </div>
        </div>
      </section>

      <section className="shell bounty-detail-grid">
        <div className="bounty-main">
          <section className="detail-panel">
            <p className="eyebrow">Acceptance contract</p>
            <ol className="criteria-list">
              {bounty.acceptanceCriteria.map((criterion, index) => (
                <li key={criterion}>
                  <span>{String(index + 1).padStart(2, '0')}</span>
                  {criterion}
                </li>
              ))}
            </ol>
            <a
              href={bounty.issueUrl}
              className="button button-secondary"
              target="_blank"
              rel="noreferrer"
            >
              Inspect source issue <span aria-hidden="true">↗</span>
            </a>
          </section>

          <section className="detail-panel transformation-receipt">
            <p className="eyebrow">Failure transformation</p>
            <div className="receipt-chain">
              <div>
                <span>01</span>
                <strong>Paid attempt stopped</strong>
                <p>
                  {bounty.failureClass?.replaceAll('_', ' ') ||
                    'Repository validation did not pass.'}
                </p>
              </div>
              <div>
                <span>02</span>
                <strong>Customer made whole</strong>
                <p>The rescue is separate from the original commercial contract.</p>
                {bounty.refundTransaction && (
                  <TransactionLink signature={bounty.refundTransaction} label="Refund" />
                )}
              </div>
              <div>
                <span>03</span>
                <strong>Rescue funds secured</strong>
                <p>The payout cannot be reused while this bounty is active.</p>
                {bounty.escrowTransaction && (
                  <TransactionLink signature={bounty.escrowTransaction} label="Funding" />
                )}
              </div>
            </div>
          </section>

          {bounty.review && (
            <section className="detail-panel">
              <p className="eyebrow">Independent review receipt</p>
              <div className="receipt-grid review-evidence">
                <div>
                  <dl className="receipt-list">
                    <div>
                      <dt>Decision</dt>
                      <dd>{bounty.review.approved ? 'passed' : 'rejected'}</dd>
                    </div>
                    <div>
                      <dt>Reviewed</dt>
                      <dd>{formatTime(bounty.review.reviewedAt)}</dd>
                    </div>
                    <div>
                      <dt>Head commitment</dt>
                      <dd className="review-commitment">{bounty.review.headSha}</dd>
                    </div>
                    <div>
                      <dt>Base commitment</dt>
                      <dd className="review-commitment">{bounty.review.baseSha}</dd>
                    </div>
                    <div>
                      <dt>Base ref</dt>
                      <dd>{bounty.review.baseRef}</dd>
                    </div>
                    <div>
                      <dt>Diff commitment</dt>
                      <dd className="review-commitment">{bounty.review.diffHash}</dd>
                    </div>
                  </dl>
                  <p className="receipt-review-reason">{bounty.review.reason}</p>
                </div>
                <div>
                  <p className="eyebrow">Provider route</p>
                  {bounty.review.provider ? (
                    <ProviderReceiptDetails receipt={bounty.review.provider} />
                  ) : (
                    <p className="receipt-empty">
                      No marketplace provider receipt is attached to this decision.
                    </p>
                  )}
                </div>
              </div>
            </section>
          )}

          {(bounty.pullRequestUrl || bounty.releaseTransaction) && (
            <section className="detail-panel">
              <p className="eyebrow">Completion evidence</p>
              <div className="completion-links">
                {bounty.pullRequestUrl && (
                  <a
                    className="receipt-link"
                    href={bounty.pullRequestUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    <span>Pull request</span>
                    <strong>Inspect merge evidence ↗</strong>
                  </a>
                )}
                {bounty.releaseTransaction && (
                  <TransactionLink signature={bounty.releaseTransaction} label="Payout" />
                )}
              </div>
            </section>
          )}
          {bounty.dispute && (
            <section className="detail-panel">
              <p className="eyebrow">Dispute receipt</p>
              <div className="dispute-receipt">
                <div>
                  <span>Status</span>
                  <strong>{stateLabel(bounty.dispute.state)}</strong>
                </div>
                <div>
                  <span>Opened</span>
                  <strong>{formatTime(bounty.dispute.openedAt)}</strong>
                </div>
                {bounty.dispute.resolution && (
                  <div>
                    <span>Evidence hash</span>
                    <strong>{bounty.dispute.resolution.evidenceHash.slice(0, 16)}…</strong>
                  </div>
                )}
              </div>
            </section>
          )}
        </div>

        <aside className="claim-panel">
          <p className="eyebrow">Claim this work</p>
          <div className="claim-step-heading">
            <span>01</span>
            <div>
              <strong>Verify your GitHub identity</strong>
              <p>Claims are tied to the contributor who will open the pull request.</p>
            </div>
          </div>
          {claimable ? (
            <>
              <GithubClaimButton bountyId={bounty.id} />
              <WalletProof bountyId={bounty.id} />
            </>
          ) : bounty.claimant ? (
            <BountyActions
              bountyId={bounty.id}
              state={bounty.state}
              claimantLogin={bounty.claimant.github}
              pullRequestUrl={bounty.pullRequestUrl}
              hasDispute={Boolean(bounty.dispute)}
            />
          ) : (
            <p className="claim-unavailable">This bounty is not accepting new claims.</p>
          )}
          <dl className="claim-rules">
            <div>
              <dt>Work lease</dt>
              <dd>48 hours, fixed at claim</dd>
            </div>
            <div>
              <dt>Deadline extensions</dt>
              <dd>None</dd>
            </div>
            <div>
              <dt>Review</dt>
              <dd>Independent route</dd>
            </div>
            <div>
              <dt>Updated</dt>
              <dd>{formatTime(bounty.updatedAt)}</dd>
            </div>
          </dl>
          {bounty.claimant && (
            <div className="current-claim">
              <span>Current claimant</span>
              <strong>@{bounty.claimant.github}</strong>
              {bounty.claimExpiresAt && (
                <small>Lease ends {formatTime(bounty.claimExpiresAt)}</small>
              )}
            </div>
          )}
          {!claimable && !bounty.claimant && (
            <p className="claim-unavailable">Status: {stateLabel(bounty.state)}.</p>
          )}
        </aside>
      </section>
    </div>
  );
}
