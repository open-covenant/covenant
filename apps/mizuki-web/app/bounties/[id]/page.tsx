import type { Metadata } from 'next';
import Link from 'next/link';
import { notFound } from 'next/navigation';
import { BountyActions } from '@/components/bounty-actions';
import { DataError, DemoNotice } from '@/components/data-state';
import { GithubClaimButton } from '@/components/github-claim-button';
import { ProviderReceiptDetails } from '@/components/provider-receipt';
import { StatusBadge } from '@/components/status-badge';
import { TransactionLink } from '@/components/transaction-link';
import { WalletProof } from '@/components/wallet-proof';
import { getBounty } from '@/lib/api';
import {
  bountyStateLabel,
  failureLabel,
  formatSolLamports,
  formatTime,
  formatUsd,
} from '@/lib/format';
import { pageMetadata } from '@/lib/page-metadata';
import type { Bounty } from '@/lib/types';
import { githubAuthErrorMessage } from '@/lib/workbench';

export const dynamic = 'force-dynamic';

export async function generateMetadata({
  params,
}: {
  params: Promise<{ id: string }>;
}): Promise<Metadata> {
  const { id } = await params;
  return pageMetadata({
    title: 'Bounty receipt',
    description:
      'Inspect the scope, claim requirements, review record, and payout evidence for a Mizuki maintenance bounty.',
    path: `/bounties/${encodeURIComponent(id)}`,
  });
}

export default async function BountyDetailPage({
  params,
  searchParams,
}: {
  params: Promise<{ id: string }>;
  searchParams?: Promise<Record<string, string | string[] | undefined>>;
}) {
  const { id } = await params;
  const query = searchParams ? await searchParams : undefined;
  const authErrorValue = Array.isArray(query?.auth_error) ? query.auth_error[0] : query?.auth_error;
  const authError = githubAuthErrorMessage(authErrorValue);
  const result = await getBounty(id);
  if (result.status === 'not_found') notFound();
  if (result.status === 'error') {
    return (
      <div className="page-shell shell fatal-state">
        <DataError title="This bounty record is temporarily unavailable" />
      </div>
    );
  }
  const bounty = result.data;
  const claimable =
    bounty.state === 'open' && Boolean(bounty.escrowTransaction && bounty.amountAtomic);
  const exactPayout =
    bounty.asset === 'SOL' && bounty.amountAtomic
      ? formatSolLamports(bounty.amountAtomic)
      : `${formatUsd(bounty.amountUsd)} ${bounty.asset || 'USDC'}`;
  return (
    <div className="page-shell">
      <section className="bounty-detail-hero shell">
        <div className="bounty-detail-topline">
          <div className="eyebrow">
            {bounty.repository} · issue #{bounty.issueNumber ?? '—'} {result.demo && <DemoNotice />}
          </div>
          <StatusBadge state={bounty.state} label={bountyStateLabel(bounty.state)} />
        </div>
        <div className="bounty-detail-title">
          <h1>{bounty.title}</h1>
          <div className="detail-price">
            <span>{payoutStatusLabel(bounty)}</span>
            <strong>{exactPayout}</strong>
            <small>
              {bounty.asset === 'SOL' ? `Approximately ${formatUsd(bounty.amountUsd)}` : ''}
            </small>
          </div>
        </div>
      </section>

      <section className="shell bounty-detail-grid">
        <div className="bounty-main">
          <section className="detail-panel">
            <p className="eyebrow">Payout requirements</p>
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
              View the authorized issue <span aria-hidden="true">↗</span>
            </a>
          </section>

          <section className="detail-panel transformation-receipt">
            <p className="eyebrow">How this bounty was funded</p>
            <div className="receipt-chain">
              <div>
                <span>01</span>
                <strong>Original job was not delivered</strong>
                <p>{failureLabel(bounty.failureClass)}</p>
              </div>
              <div>
                <span>02</span>
                <strong>Quoted USDC payment refunded</strong>
                <p>The original paid job ended before this separate bounty was funded.</p>
                {bounty.customerRefundTransaction && (
                  <TransactionLink
                    signature={bounty.customerRefundTransaction}
                    label="Customer refund"
                  />
                )}
              </div>
              <div>
                <span>03</span>
                <strong>
                  {bounty.releaseTransaction
                    ? 'SOL payout released'
                    : bounty.escrowReturnTransaction
                      ? 'SOL escrow returned'
                      : escrowReturnPending(bounty.state)
                        ? 'SOL escrow return pending'
                        : 'SOL payout secured'}
                </strong>
                <p>
                  {bounty.releaseTransaction
                    ? 'The dedicated SOL payout was released to the approved contributor.'
                    : bounty.escrowReturnTransaction
                      ? 'The dedicated SOL bounty funds were returned from escrow.'
                      : escrowReturnPending(bounty.state)
                        ? 'The dedicated SOL bounty funds remain in escrow until the return finalizes.'
                        : 'The payout is held in dedicated on-chain escrow for this bounty.'}
                </p>
                {bounty.escrowTransaction && (
                  <TransactionLink signature={bounty.escrowTransaction} label="Funding" />
                )}
                {bounty.escrowReturnTransaction && (
                  <TransactionLink
                    signature={bounty.escrowReturnTransaction}
                    label="Escrow return"
                  />
                )}
              </div>
            </div>
          </section>

          {bounty.review && (
            <section className="detail-panel">
              <p className="eyebrow">Separate AI review record</p>
              <div className="receipt-grid review-evidence">
                <div>
                  <dl className="receipt-list">
                    <div>
                      <dt>Decision</dt>
                      <dd>{bounty.review.approved ? 'Approved' : 'Not approved'}</dd>
                    </div>
                    <div>
                      <dt>Reviewed</dt>
                      <dd>{formatTime(bounty.review.reviewedAt)}</dd>
                    </div>
                    <div>
                      <dt>Reviewed commit</dt>
                      <dd className="review-commitment">{bounty.review.headSha}</dd>
                    </div>
                    <div>
                      <dt>Base commit</dt>
                      <dd className="review-commitment">{bounty.review.baseSha}</dd>
                    </div>
                    <div>
                      <dt>Target branch</dt>
                      <dd>{bounty.review.baseRef}</dd>
                    </div>
                    <div>
                      <dt>Reviewed diff hash</dt>
                      <dd className="review-commitment">{bounty.review.diffHash}</dd>
                    </div>
                  </dl>
                  <p className="receipt-review-reason">{bounty.review.reason}</p>
                  <p className="receipt-empty">
                    This separate AI review is not a human review, maintainer approval, or security
                    audit.
                  </p>
                </div>
                <div>
                  <p className="eyebrow">AI provider receipt</p>
                  {bounty.review.provider ? (
                    <ProviderReceiptDetails receipt={bounty.review.provider} />
                  ) : (
                    <p className="receipt-empty">
                      No AI provider receipt is attached to this review.
                    </p>
                  )}
                </div>
              </div>
            </section>
          )}

          {(bounty.pullRequestUrl || bounty.releaseTransaction) && (
            <section className="detail-panel">
              <p className="eyebrow">Completion records</p>
              <div className="completion-links">
                {bounty.pullRequestUrl && (
                  <a
                    className="receipt-link"
                    href={bounty.pullRequestUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    <span>Pull request</span>
                    <strong>View pull request on GitHub ↗</strong>
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
              <p className="eyebrow">Dispute record</p>
              <div className="dispute-receipt">
                <div>
                  <span>Status</span>
                  <strong>{disputeStateLabel(bounty.dispute.state)}</strong>
                </div>
                <div>
                  <span>Opened</span>
                  <strong>{formatTime(bounty.dispute.openedAt)}</strong>
                </div>
                {bounty.dispute.resolution && (
                  <>
                    <div>
                      <span>Requested outcome</span>
                      <strong>
                        {disputeDecisionLabel(bounty.dispute.resolution.requestedDecision)}
                      </strong>
                    </div>
                    <div>
                      <span>Final outcome</span>
                      <strong>
                        {disputeDecisionLabel(bounty.dispute.resolution.settlementDecision)}
                      </strong>
                    </div>
                    <div>
                      <span>Decision recorded</span>
                      <strong>{formatTime(bounty.dispute.resolution.decidedAt)}</strong>
                    </div>
                    {bounty.dispute.resolution.resolvedAt && (
                      <div>
                        <span>Settlement finalized</span>
                        <strong>{formatTime(bounty.dispute.resolution.resolvedAt)}</strong>
                      </div>
                    )}
                  </>
                )}
              </div>
              {bounty.dispute.resolution && (
                <div className="dispute-evidence">
                  <p>{bounty.dispute.resolution.summary}</p>
                  <dl className="receipt-list">
                    <div>
                      <dt>Evidence hash</dt>
                      <dd className="review-commitment">
                        {bounty.dispute.resolution.evidenceHash}
                      </dd>
                    </div>
                  </dl>
                  <div className="completion-links">
                    {bounty.dispute.resolution.references.map((reference, index) => (
                      <a
                        className="receipt-link"
                        href={reference}
                        target="_blank"
                        rel="noreferrer"
                        key={reference}
                      >
                        <span>Evidence {index + 1}</span>
                        <strong>Open supporting record ↗</strong>
                      </a>
                    ))}
                    {bounty.dispute.resolution.transactionSignature && (
                      <TransactionLink
                        signature={bounty.dispute.resolution.transactionSignature}
                        label="Dispute settlement"
                      />
                    )}
                  </div>
                </div>
              )}
            </section>
          )}
        </div>

        <aside className="claim-panel">
          {authError && (
            <p className="form-error" role="alert">
              {authError}
            </p>
          )}
          {claimable ? (
            <>
              <p className="eyebrow">Claim this work</p>
              <div className="claim-step-heading">
                <span>01</span>
                <div>
                  <strong>Sign in with GitHub</strong>
                  <p>Use the GitHub account that will submit the pull request.</p>
                </div>
              </div>
              <p className="claim-contract">
                Claiming reserves the bounty for 48 hours. Payout requires every listed criterion,
                separate AI review, approval from a non-claimant maintainer on the exact reviewed
                commit, and merge before the claim expires.
              </p>
              <p className="claim-consent">
                By claiming, you agree to the <Link href="/terms">Service terms</Link>.
              </p>
              <GithubClaimButton bountyId={bounty.id} />
              <WalletProof bountyId={bounty.id} />
              <ClaimRules updatedAt={bounty.updatedAt} />
            </>
          ) : bounty.claimant && !terminalBountyState(bounty.state) ? (
            <>
              <p className="eyebrow">Contributor workflow</p>
              <div className="current-claim">
                <span>Assigned contributor</span>
                <strong>@{bounty.claimant.github}</strong>
                {bounty.claimExpiresAt && (
                  <small>Claim window ends {formatTime(bounty.claimExpiresAt)}</small>
                )}
              </div>
              <p className="claim-contract">
                Payout requires every listed criterion, separate AI review, approval from a
                non-claimant maintainer on the exact reviewed commit, and merge before the claim
                expires.
              </p>
              <BountyActions
                bountyId={bounty.id}
                state={bounty.state}
                claimantLogin={bounty.claimant.github}
                pullRequestUrl={bounty.pullRequestUrl}
                hasDispute={Boolean(bounty.dispute)}
              />
              <ClaimRules updatedAt={bounty.updatedAt} />
            </>
          ) : (
            <>
              <p className="eyebrow">
                {terminalBountyState(bounty.state) ? 'Bounty outcome' : 'Bounty status'}
              </p>
              <div className="closed-bounty-status">
                <strong>{bountyStateSummary(bounty)}</strong>
                <p>{bountyStateExplanation(bounty)}</p>
              </div>
              {bounty.claimant && (
                <div className="current-claim">
                  <span>Contributor of record</span>
                  <strong>@{bounty.claimant.github}</strong>
                </div>
              )}
              <dl className="claim-rules">
                <div>
                  <dt>Last updated</dt>
                  <dd>{formatTime(bounty.updatedAt)}</dd>
                </div>
              </dl>
            </>
          )}
        </aside>
      </section>
    </div>
  );
}

function ClaimRules({ updatedAt }: { updatedAt: string }) {
  return (
    <dl className="claim-rules">
      <div>
        <dt>Claim window</dt>
        <dd>48 hours from claim acceptance</dd>
      </div>
      <div>
        <dt>Extensions</dt>
        <dd>Not available</dd>
      </div>
      <div>
        <dt>Payout review</dt>
        <dd>Checks, separate AI review, maintainer approval</dd>
      </div>
      <div>
        <dt>Last updated</dt>
        <dd>{formatTime(updatedAt)}</dd>
      </div>
    </dl>
  );
}

function payoutStatusLabel(bounty: Bounty): string {
  if (bounty.releaseTransaction || bounty.state === 'released') return 'Paid to contributor';
  if (bounty.escrowReturnTransaction || bounty.state === 'refunded' || bounty.state === 'expired') {
    return 'Returned from escrow';
  }
  if (escrowReturnPending(bounty.state)) return 'Escrow return pending';
  if (bounty.state === 'rejected') return 'Payout not released';
  if (bounty.state === 'accepted') return 'Payout release pending';
  if (bounty.state === 'disputed') return 'Payout on hold';
  return bounty.escrowTransaction ? 'Escrowed payout' : 'Funding pending';
}

function escrowReturnPending(state: Bounty['state']): boolean {
  return ['claim_refund_pending', 'offer_refund_pending', 'release_refund_pending'].includes(state);
}

function terminalBountyState(state: Bounty['state']): boolean {
  return ['released', 'expired', 'rejected', 'refunded'].includes(state);
}

function bountyStateSummary(bounty: Bounty): string {
  return bountyStateLabel(bounty.state);
}

function bountyStateExplanation(bounty: Bounty): string {
  if (bounty.state === 'released') {
    return 'The approved contributor payout finalized. The payout transaction is shown in the completion records.';
  }
  if (bounty.state === 'expired') {
    return 'The bounty offer ended without an accepted contribution, and the dedicated SOL funds were returned from escrow.';
  }
  if (bounty.state === 'refunded') {
    return 'The contribution did not complete the payout process, and the dedicated SOL funds were returned from escrow.';
  }
  if (bounty.state === 'rejected') {
    return 'The submitted work did not meet the payout requirements. No contributor payout was released.';
  }
  if (escrowReturnPending(bounty.state)) {
    return 'The bounty is closed to new work while the dedicated SOL funds are returned from escrow.';
  }
  return 'This bounty is not accepting new claims. Review the public records on this page for its current status.';
}

function disputeDecisionLabel(decision: 'release' | 'refund'): string {
  return decision === 'release' ? 'Release contributor payout' : 'Return SOL escrow';
}

function disputeStateLabel(state: NonNullable<Bounty['dispute']>['state']): string {
  const labels = {
    open: 'Under review',
    release_pending: 'Contributor payout pending',
    refund_pending: 'SOL escrow return pending',
    released: 'Contributor payout completed',
    refunded: 'SOL escrow returned',
  } as const;
  return labels[state];
}
