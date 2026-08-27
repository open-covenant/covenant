import { useState } from 'react';

import type { ComputeApp, ComputeJob, JobStatus } from '../domain';
import {
  formatDuration,
  formatUsdc,
  shortId,
  terminalStatuses,
} from '../domain';
import { Icon } from './Icon';

interface JobPanelProps {
  app?: ComputeApp;
  busy: boolean;
  job: ComputeJob;
  onCancel: () => Promise<void>;
  onOpen: () => Promise<void>;
}

const statusCopy: Record<JobStatus, string> = {
  funding: 'Reserving the private-beta allowance',
  provisioning: 'Preparing your dedicated workspace',
  running: 'Workspace is ready',
  stopping: 'Stopping the workload safely',
  completed: 'Workload completed',
  cancelled: 'Workload cancelled',
  failed: 'Workload failed',
};

const progress: JobStatus[] = ['funding', 'provisioning', 'running'];

export const stopStorageCopy = {
  idle: 'Workspace storage is temporary and deleted when stopped. Download your work first.',
  armed: 'Stopping permanently deletes workspace storage. Download your work before confirming.',
} as const;

export function JobPanel({ app, busy, job, onCancel, onOpen }: JobPanelProps) {
  const [cancelArmed, setCancelArmed] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const terminal = terminalStatuses.has(job.status);
  const currentProgress = progress.indexOf(job.status);
  const hasSettlement = job.receipt?.transaction != null;

  async function copy(label: string, value: string) {
    await navigator.clipboard.writeText(value);
    setCopied(label);
    window.setTimeout(() => setCopied(null), 1_500);
  }

  async function cancel() {
    if (!cancelArmed) {
      setCancelArmed(true);
      return;
    }
    await onCancel();
    setCancelArmed(false);
  }

  return (
    <section className="job-panel" aria-live="polite">
      <div className="job-panel__header">
        <div>
          <p className="eyebrow">Current workload</p>
          <h2>{app?.name ?? 'GPU workload'}</h2>
        </div>
        <span className={`job-status job-status--${job.status}`}>
          <span className="status-dot" />
          {job.status}
        </span>
      </div>

      <div className="job-panel__identity">
        <code>{shortId(job.id, 9)}</code>
        <button
          aria-label="Copy workload ID"
          className="icon-button"
          onClick={() => void copy('job', job.id)}
          type="button"
        >
          <Icon name={copied === 'job' ? 'check' : 'copy'} />
        </button>
      </div>

      {!terminal && (
        <>
          <div className="job-progress" aria-label={statusCopy[job.status]}>
            {progress.map((status, index) => {
              const complete = currentProgress >= index || job.status === 'stopping';
              return (
                <span
                  className={`job-progress__step${complete ? ' job-progress__step--complete' : ''}`}
                  key={status}
                />
              );
            })}
          </div>
          <p className="job-panel__status-copy">{statusCopy[job.status]}</p>
        </>
      )}

      {job.status === 'running' && job.access_ready && (
        <button
          className="primary-button primary-button--access"
          onClick={() => void onOpen()}
          type="button"
        >
          Open workspace
          <Icon name="external" />
        </button>
      )}

      {job.error && <p className="inline-alert inline-alert--error">{job.error}</p>}

      {!terminal && (
        <div className="job-panel__actions">
          <button
            className={`cancel-button${cancelArmed ? ' cancel-button--armed' : ''}`}
            disabled={busy || job.status === 'stopping'}
            onBlur={() => setCancelArmed(false)}
            onClick={() => void cancel()}
            type="button"
          >
            <Icon name="stop" />
            {busy
              ? 'Stopping…'
              : cancelArmed
                ? 'Confirm stop and delete'
                : job.status === 'stopping'
                  ? 'Stopping'
                  : 'Stop workload'}
          </button>
          <p className={cancelArmed ? 'job-panel__stop-warning--armed' : undefined}>
            {cancelArmed ? stopStorageCopy.armed : stopStorageCopy.idle}
          </p>
        </div>
      )}

      {job.receipt && (
        <div className="receipt">
          <div className="receipt__title">
            <span className="receipt__icon">
              <Icon name="receipt" />
            </span>
            <div>
              <p className="eyebrow">
                {hasSettlement ? 'Settlement receipt' : 'Usage evidence'}
              </p>
              <h3>
                {hasSettlement ? 'Payment accounted for' : 'Beta allowance accounted for'}
              </h3>
            </div>
          </div>
          <dl className="receipt__totals">
            <div>
              <dt>Runtime</dt>
              <dd>{formatDuration(job.receipt.runtime_secs)}</dd>
            </div>
            <div>
              <dt>{hasSettlement ? 'Charged' : 'Allowance used'}</dt>
              <dd>{formatUsdc(job.receipt.charged_usdc_micros)}</dd>
            </div>
            <div>
              <dt>{hasSettlement ? 'Returned' : 'Allowance released'}</dt>
              <dd>{formatUsdc(job.receipt.refunded_usdc_micros)}</dd>
            </div>
          </dl>
          <div className="receipt__commitment">
            <span>
              <small>Commitment</small>
              <code>{shortId(job.receipt.commitment, 10)}</code>
            </span>
            <button
              aria-label="Copy receipt commitment"
              className="icon-button"
              onClick={() => void copy('receipt', job.receipt!.commitment)}
              type="button"
            >
              <Icon name={copied === 'receipt' ? 'check' : 'copy'} />
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
