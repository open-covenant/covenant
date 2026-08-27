'use client';

import { useEffect, useRef, useState } from 'react';
import { formatTime, formatUsdcAtomic, formatUsd, stateLabel } from '@/lib/format';
import type { Job, ReviewAttempt } from '@/lib/types';
import { fetchWithDeadline } from '@/lib/workbench-client';
import { ProviderReceiptDetails } from './provider-receipt';

const stages = ['paid', 'running', 'validating', 'delivered'] as const;
const pollTimeoutMs = 12_000;

function stagePosition(state: Job['state']): number {
  if (state === 'quoted' || state === 'settlement_pending' || state === 'payment_expired')
    return -1;
  if (state === 'paid' || state === 'admitted') return 0;
  if (state === 'running') return 1;
  if (state === 'validating') return 2;
  if (state === 'delivered') return 3;
  return 0;
}

export function JobReceipt({ initial, live = true }: { initial: Job; live?: boolean }) {
  const [job, setJob] = useState(initial);
  const [pollError, setPollError] = useState<string | null>(null);
  const currentJob = useRef(initial);

  useEffect(() => {
    currentJob.current = job;
  }, [job]);

  useEffect(() => {
    if (!live || jobPollingComplete(currentJob.current)) return;
    let stopped = false;
    let timer: number | undefined;
    let controller: AbortController | undefined;

    async function poll() {
      controller = new AbortController();
      try {
        const response = await fetchWithDeadline(
          `/api/mizuki/v1/jobs/${encodeURIComponent(initial.id)}`,
          { cache: 'no-store', signal: controller.signal },
          fetch,
          pollTimeoutMs,
        );
        const body = (await response.json()) as Job;
        if (!response.ok) throw new Error('Status refresh failed');
        if (shouldApplyJobUpdate(currentJob.current, body)) {
          currentJob.current = body;
          setJob(body);
        }
        setPollError(null);
      } catch (cause) {
        if (stopped || (cause instanceof DOMException && cause.name === 'AbortError')) return;
        setPollError('Status refresh failed');
      } finally {
        controller = undefined;
        if (!stopped && !jobPollingComplete(currentJob.current)) {
          timer = window.setTimeout(poll, 5_000);
        }
      }
    }

    timer = window.setTimeout(poll, 5_000);
    return () => {
      stopped = true;
      if (timer !== undefined) window.clearTimeout(timer);
      controller?.abort();
    };
  }, [initial.id, live]);

  const progress = stagePosition(job.state);
  const paymentExpired = job.state === 'payment_expired';
  const failed =
    paymentExpired || ['rejected', 'failed', 'refund_pending', 'refunded'].includes(job.state);
  const state = job.mergedAt ? 'Merged' : stateLabel(job.state);

  return (
    <div className="job-receipt">
      <div className="job-status-panel">
        <div className="job-status-heading">
          <div>
            <span>Current state</span>
            <strong className={failed ? 'state-failed' : ''}>{state}</strong>
          </div>
          {live && !jobPollingComplete(job) && (
            <span className="processing-indicator">
              <i aria-hidden="true" /> Live
            </span>
          )}
        </div>
        {!failed ? (
          <ol className="job-progress">
            {stages.map((stage, index) => (
              <li className={index <= progress ? 'complete' : ''} key={stage}>
                <span>
                  {index < progress || (stage === 'delivered' && job.mergedAt)
                    ? '✓'
                    : String(index + 1).padStart(2, '0')}
                </span>
                {stage === 'delivered' && job.mergedAt ? 'Merged' : stateLabel(stage)}
              </li>
            ))}
          </ol>
        ) : paymentExpired ? (
          <div className="refund-path">
            <span>Payment authorization</span>
            <span aria-hidden="true">→</span>
            <strong>No payment settled</strong>
          </div>
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

      {job.deliveryEvidence && (
        <div className="receipt-grid review-evidence">
          <section>
            <p className="eyebrow">Delivery commitment</p>
            <dl className="receipt-list">
              <div>
                <dt>Pull request</dt>
                <dd>#{job.deliveryEvidence.pullRequestNumber}</dd>
              </div>
              <div>
                <dt>Delivered head</dt>
                <dd className="review-commitment">{job.deliveryEvidence.headSha}</dd>
              </div>
              <div>
                <dt>Quoted base</dt>
                <dd className="review-commitment">
                  {job.deliveryEvidence.baseRef} · {job.deliveryEvidence.baseSha}
                </dd>
              </div>
              <div>
                <dt>Canonical patch hash</dt>
                <dd className="review-commitment">{job.deliveryEvidence.diffHash}</dd>
              </div>
              <div>
                <dt>Recorded</dt>
                <dd>{formatTime(job.deliveryEvidence.observedAt)}</dd>
              </div>
            </dl>
          </section>
          <section>
            <p className="eyebrow">Merge and refund-liability evidence</p>
            <dl className="receipt-list">
              <div>
                <dt>Repository outcome</dt>
                <dd>{job.mergedAt ? `Merged ${formatTime(job.mergedAt)}` : 'Awaiting merge'}</dd>
              </div>
              <div>
                <dt>Refund liability</dt>
                <dd>
                  {job.refundLiabilityDischarge
                    ? `Discharged ${formatTime(job.refundLiabilityDischarge.dischargedAt)}`
                    : 'Active until a qualifying merge or refund'}
                </dd>
              </div>
              {job.refundLiabilityDischarge && (
                <div>
                  <dt>Policy evidence hash</dt>
                  <dd className="review-commitment">{job.refundLiabilityDischarge.evidenceHash}</dd>
                </div>
              )}
            </dl>
            <p className="receipt-empty">
              The separate policy signer verifies the required exact-head approval and repository
              merge before it can discharge the refund liability.
            </p>
          </section>
        </div>
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
            {paymentExpired
              ? 'The payment authorization expired without a finalized charge. No refund is required.'
              : job.refundTransaction
                ? 'The full quoted USDC payment was returned to the original payer. A maintenance bounty is published only after separate SOL escrow funding succeeds.'
                : 'The refund has not finalized yet. No maintenance bounty will be offered until it does.'}
          </strong>
        </div>
      )}
    </div>
  );
}

export function shouldApplyJobUpdate(current: Job, next: Job): boolean {
  if (current.id !== next.id || jobPollingComplete(current)) return false;
  const currentUpdatedAt = Date.parse(current.updatedAt);
  const nextUpdatedAt = Date.parse(next.updatedAt);
  if (!Number.isFinite(nextUpdatedAt)) return false;
  return !Number.isFinite(currentUpdatedAt) || nextUpdatedAt > currentUpdatedAt;
}

export function jobPollingComplete(job: Job): boolean {
  if (job.state === 'payment_expired') return true;
  if (job.state === 'refunded') return Boolean(job.refundTransaction);
  return Boolean(
    job.state === 'delivered' &&
    job.mergedAt &&
    job.refundLiabilityDischarge?.dischargedAt &&
    job.refundLiabilityDischarge.evidenceHash,
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
