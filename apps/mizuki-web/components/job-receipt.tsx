'use client';

import { useEffect, useState } from 'react';
import { formatTime, formatUsdcAtomic, formatUsd, stateLabel } from '@/lib/format';
import type { Job, ReviewAttempt } from '@/lib/types';
import { ProviderReceiptDetails } from './provider-receipt';

const terminal = new Set(['delivered', 'rejected', 'failed', 'refunded']);
const stages = ['paid', 'running', 'validating', 'delivered'] as const;

function stagePosition(state: Job['state']): number {
  if (state === 'quoted' || state === 'settlement_pending') return -1;
  if (state === 'paid' || state === 'admitted') return 0;
  if (state === 'running') return 1;
  if (state === 'validating') return 2;
  if (state === 'delivered') return 3;
  return 0;
}

export function JobReceipt({ initial, live = true }: { initial: Job; live?: boolean }) {
  const [job, setJob] = useState(initial);
  const [pollError, setPollError] = useState<string | null>(null);

  useEffect(() => {
    if (!live || terminal.has(job.state)) return;
    const poll = window.setInterval(async () => {
      try {
        const response = await fetch(`/api/mizuki/v1/jobs/${encodeURIComponent(job.id)}`, {
          cache: 'no-store',
        });
        const body = (await response.json()) as Job;
        if (!response.ok) throw new Error('Status refresh failed');
        setJob(body);
        setPollError(null);
      } catch {
        setPollError('Status refresh failed');
      }
    }, 5_000);
    return () => window.clearInterval(poll);
  }, [job.id, job.state, live]);

  const progress = stagePosition(job.state);
  const failed = ['rejected', 'failed', 'refund_pending', 'refunded'].includes(job.state);

  return (
    <div className="job-receipt">
      <div className="job-status-panel">
        <div className="job-status-heading">
          <div>
            <span>Current state</span>
            <strong className={failed ? 'state-failed' : ''}>{stateLabel(job.state)}</strong>
          </div>
          {live && !terminal.has(job.state) && (
            <span className="processing-indicator">
              <i aria-hidden="true" /> Live
            </span>
          )}
        </div>
        {!failed ? (
          <ol className="job-progress">
            {stages.map((stage, index) => (
              <li className={index <= progress ? 'complete' : ''} key={stage}>
                <span>{index < progress ? '✓' : String(index + 1).padStart(2, '0')}</span>
                {stateLabel(stage)}
              </li>
            ))}
          </ol>
        ) : (
          <div className="refund-path">
            <span>Paid attempt</span>
            <span aria-hidden="true">→</span>
            <span>No qualifying pull request opened</span>
            <span aria-hidden="true">→</span>
            <strong>
              {job.state === 'refunded' ? 'Full refund finalized' : 'Full refund in progress'}
            </strong>
          </div>
        )}
        {pollError && (
          <p className="poll-warning">
            Live updates are temporarily unavailable. Refresh the page to check the latest status.
            No payment or refund will be repeated.
          </p>
        )}
      </div>

      <div className="receipt-grid">
        <section>
          <p className="eyebrow">Job details</p>
          <dl className="receipt-list">
            <div>
              <dt>Issue</dt>
              <dd>
                <a href={job.issueUrl} target="_blank" rel="noreferrer">
                  Open on GitHub ↗
                </a>
              </dd>
            </div>
            <div>
              <dt>Service level</dt>
              <dd>{stateLabel(job.class)}</dd>
            </div>
            <div>
              <dt>Quoted amount</dt>
              <dd>{formatUsdcAtomic(job.priceAtomic)}</dd>
            </div>
            <div>
              <dt>Estimated compute cost</dt>
              <dd>{formatUsd(job.variableRouteCostEstimateUsd)}</dd>
            </div>
            <div>
              <dt>Estimate coverage</dt>
              <dd>
                Includes estimated AI model usage and isolated code-execution time. Excludes
                provider billing adjustments, Solana and payment fees, and hosting costs.
              </dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>{formatTime(job.createdAt)}</dd>
            </div>
            <div>
              <dt>Updated</dt>
              <dd>{formatTime(job.updatedAt)}</dd>
            </div>
          </dl>
        </section>

        <section>
          <p className="eyebrow">Patch and check results</p>
          {job.changedFiles.length > 0 ? (
            <ul className="file-list">
              {job.changedFiles.map((file) => (
                <li key={file}>{file}</li>
              ))}
            </ul>
          ) : (
            <p className="receipt-empty">Changed files will appear when a patch is available.</p>
          )}
          {job.validations.length > 0 && (
            <ul className="validation-list">
              {job.validations.map((validation) => (
                <li key={validation.command}>
                  <span>{validation.exitCode === 0 ? '✓' : '!'}</span>
                  <code>{validation.command}</code>
                  <strong>
                    {validation.exitCode === 0
                      ? 'Passed'
                      : `Failed (exit code ${validation.exitCode})`}
                  </strong>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>

      {job.review && (
        <div className="receipt-grid review-evidence">
          <section>
            <p className="eyebrow">Separate AI review record</p>
            <dl className="receipt-list">
              <div>
                <dt>Decision</dt>
                <dd>{job.review.approved ? 'Approved' : 'Not approved'}</dd>
              </div>
              <div>
                <dt>Reviewed</dt>
                <dd>{formatTime(job.review.reviewedAt)}</dd>
              </div>
              <div>
                <dt>Reviewed patch hash</dt>
                <dd className="review-commitment">{job.review.artifactHash}</dd>
              </div>
            </dl>
            <p className="receipt-review-reason">{job.review.reason}</p>
            <p className="receipt-empty">
              This separate AI review is not a human review, maintainer approval, or security audit.
            </p>
            {failed && (
              <p className="receipt-empty">
                This review finished before the later delivery failure. It records provider work,
                not a successful delivery.
              </p>
            )}
          </section>
          <section>
            <p className="eyebrow">AI provider receipt</p>
            {job.review.provider ? (
              <ProviderReceiptDetails receipt={job.review.provider} />
            ) : (
              <p className="receipt-empty">No AI provider receipt is attached to this review.</p>
            )}
          </section>
        </div>
      )}

      {job.reviewAttempts && job.reviewAttempts.length > 0 && (
        <section className="review-attempts">
          <p className="eyebrow">AI review history</p>
          <div className="review-attempt-grid">
            {job.reviewAttempts.map((attempt, index) => (
              <article
                className="review-attempt"
                key={`${attempt.phase}-${attempt.reviewedAt}-${index}`}
              >
                <dl className="receipt-list">
                  <div>
                    <dt>Phase</dt>
                    <dd>
                      {attempt.phase === 'implementation' ? 'Initial review' : 'Follow-up review'}
                    </dd>
                  </div>
                  <div>
                    <dt>Status</dt>
                    <dd>{reviewAttemptLabel(attempt)}</dd>
                  </div>
                  <div>
                    <dt>Recorded</dt>
                    <dd>{formatTime(attempt.reviewedAt)}</dd>
                  </div>
                  <div>
                    <dt>Recorded review cost</dt>
                    <dd>{formatUsd(attempt.costUsd)}</dd>
                  </div>
                  <div>
                    <dt>Patch hash</dt>
                    <dd className="review-commitment">{attempt.artifactHash}</dd>
                  </div>
                </dl>
                <p className="receipt-review-reason">{attempt.reason}</p>
                {attempt.provider && <ProviderReceiptDetails receipt={attempt.provider} />}
              </article>
            ))}
          </div>
        </section>
      )}

      <div className="receipt-actions">
        {job.paymentTransaction && (
          <TransactionLinkClient signature={job.paymentTransaction} label="Payment" />
        )}
        {job.refundTransaction && (
          <TransactionLinkClient signature={job.refundTransaction} label="Full refund" />
        )}
        {job.prUrl && (
          <a className="receipt-link" href={job.prUrl} target="_blank" rel="noreferrer">
            <span>Pull request</span>
            <strong>Inspect on GitHub ↗</strong>
          </a>
        )}
      </div>

      {job.error && (
        <div className="failure-receipt">
          <span>Job outcome</span>
          <p>{job.error}</p>
          <strong>
            {job.refundTransaction
              ? 'The full quoted USDC payment was returned to the original payer. A maintenance bounty is published only after separate SOL escrow funding succeeds.'
              : 'The refund has not finalized yet. No maintenance bounty will be offered until it does.'}
          </strong>
        </div>
      )}
    </div>
  );
}

function reviewAttemptLabel(attempt: ReviewAttempt): string {
  if (attempt.status === 'pending') return 'Pending';
  if (attempt.status === 'received') return 'Provider receipt recorded';
  if (attempt.status === 'failed') return 'Review could not complete';
  if (attempt.approved === true) return 'Approved';
  if (attempt.approved === false) return 'Not approved';
  return 'Completed';
}

function TransactionLinkClient({ signature, label }: { signature: string; label: string }) {
  const base = process.env.NEXT_PUBLIC_SOLANA_EXPLORER_URL || 'https://solscan.io';
  const cluster =
    process.env.NEXT_PUBLIC_SOLANA_NETWORK === 'solana-devnet' ? '?cluster=devnet' : '';
  return (
    <a
      className="receipt-link"
      href={`${base.replace(/\/$/, '')}/tx/${encodeURIComponent(signature)}${cluster}`}
      target="_blank"
      rel="noreferrer"
    >
      <span>{label}</span>
      <strong>Inspect transaction ↗</strong>
    </a>
  );
}
