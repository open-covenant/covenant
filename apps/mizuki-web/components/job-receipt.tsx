'use client';

import { useEffect, useState } from 'react';
import { formatTime, formatUsdcAtomic, formatUsd, stateLabel } from '@/lib/format';
import type { Job } from '@/lib/types';

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
        const body = (await response.json()) as Job & { error?: string };
        if (!response.ok) throw new Error(body.error || `Status returned ${response.status}`);
        setJob(body);
        setPollError(null);
      } catch (cause) {
        setPollError(cause instanceof Error ? cause.message : 'Status refresh failed');
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
            <span>Stopped safely</span>
            <span aria-hidden="true">→</span>
            <strong>
              {job.state === 'refunded' ? 'Full refund finalized' : 'Full refund in progress'}
            </strong>
          </div>
        )}
        {pollError && (
          <p className="poll-warning">
            Live refresh interrupted: {pollError}. The financial operation is not repeated.
          </p>
        )}
      </div>

      <div className="receipt-grid">
        <section>
          <p className="eyebrow">Job contract</p>
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
              <dt>Scope</dt>
              <dd>{job.class}</dd>
            </div>
            <div>
              <dt>Price paid</dt>
              <dd>{formatUsdcAtomic(job.priceAtomic)} USDC</dd>
            </div>
            <div>
              <dt>Variable route estimate</dt>
              <dd>{formatUsd(job.variableRouteCostEstimateUsd)}</dd>
            </div>
            <div>
              <dt>Cost coverage</dt>
              <dd>
                Model and sandbox estimates included; provider adjustments, chain/facilitator, and
                infrastructure excluded
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
          <p className="eyebrow">Execution evidence</p>
          {job.changedFiles.length > 0 ? (
            <ul className="file-list">
              {job.changedFiles.map((file) => (
                <li key={file}>{file}</li>
              ))}
            </ul>
          ) : (
            <p className="receipt-empty">Changed files will appear after the coding run.</p>
          )}
          {job.validations.length > 0 && (
            <ul className="validation-list">
              {job.validations.map((validation) => (
                <li key={validation.command}>
                  <span>{validation.exitCode === 0 ? '✓' : '!'}</span>
                  <code>{validation.command}</code>
                  <strong>
                    {validation.exitCode === 0 ? 'passed' : `exit ${validation.exitCode}`}
                  </strong>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>

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
          <span>Failure receipt</span>
          <p>{job.error}</p>
          <strong>
            {job.refundTransaction
              ? 'Refund finalized. Rescue bounty generation follows independently.'
              : 'Refund processing is isolated from bounty funding.'}
          </strong>
        </div>
      )}
    </div>
  );
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
