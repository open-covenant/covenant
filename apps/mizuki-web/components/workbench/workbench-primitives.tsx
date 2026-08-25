'use client';

import Link from 'next/link';
import { formatTime, formatUsdcAtomic, relativeTime, stateLabel } from '@/lib/format';
import type { InstallationStatus, WorkbenchJob, WorkbenchRepository } from '@/lib/workbench';
import { jobIssueNumber, jobRepository } from '@/lib/workbench';

export function WorkbenchPageHeader({
  eyebrow,
  title,
  description,
  action,
}: {
  eyebrow?: string;
  title: string;
  description: string;
  action?: React.ReactNode;
}) {
  return (
    <header className="workbench-page-header">
      <div>
        {eyebrow && <p>{eyebrow}</p>}
        <h1>{title}</h1>
        <span>{description}</span>
      </div>
      {action && <div className="workbench-page-action">{action}</div>}
    </header>
  );
}

export function WorkbenchStatus({ value }: { value: string }) {
  const tone =
    value === 'ready' ||
    value === 'delivered' ||
    value === 'refunded' ||
    value === 'released' ||
    value === 'finalized'
      ? 'positive'
      : value === 'action_required' ||
          value === 'unavailable' ||
          value === 'refund_pending' ||
          value === 'pending'
        ? 'warning'
        : value === 'unsupported' || value === 'rejected' || value === 'failed'
          ? 'negative'
          : 'neutral';
  const labels: Record<string, string> = {
    action_required: 'Action required',
    checking: 'Checking',
    failed: 'Failed',
    finalized: 'Finalized',
    pending: 'Pending',
    ready: 'Ready',
    unavailable: 'Temporarily unavailable',
    unsupported: 'Unsupported',
  };
  const label = labels[value] ?? stateLabel(value);
  return <span className={`workbench-status ${tone}`}>{label}</span>;
}

export function WorkbenchLoading({ label = 'Loading' }: { label?: string }) {
  return (
    <div className="workbench-loading" aria-busy="true" aria-label={label}>
      <div className="workbench-skeleton summary-skeleton" />
      <div className="workbench-skeleton summary-skeleton" />
      <div className="workbench-skeleton summary-skeleton" />
      <div className="workbench-skeleton list-skeleton" />
    </div>
  );
}

export function WorkbenchEmpty({
  title,
  detail,
  action,
}: {
  title: string;
  detail: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="workbench-empty">
      <span aria-hidden="true">0</span>
      <div>
        <strong>{title}</strong>
        <p>{detail}</p>
        {action && <div className="workbench-empty-action">{action}</div>}
      </div>
    </div>
  );
}

export function WorkbenchError({
  title,
  detail,
  retry,
}: {
  title: string;
  detail?: string;
  retry?: () => void;
}) {
  return (
    <div className="workbench-error" role="alert">
      <span aria-hidden="true">!</span>
      <div>
        <strong>{title}</strong>
        <p>{detail || 'No payment or repository action was attempted.'}</p>
        {retry && (
          <button type="button" onClick={retry}>
            Try again
          </button>
        )}
      </div>
    </div>
  );
}

export function SummaryCard({
  label,
  value,
  detail,
}: {
  label: string;
  value: string | number;
  detail: string;
}) {
  return (
    <article className="workbench-summary-card">
      <span>{label}</span>
      <strong>{value}</strong>
      <p>{detail}</p>
    </article>
  );
}

export function RepositoryCard({
  repository,
  retry,
}: {
  repository: WorkbenchRepository;
  retry?: () => void;
}) {
  const href = `/app/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}`;
  return (
    <article className="workbench-repository-card">
      <div className="workbench-card-heading">
        <div className="repository-mark" aria-hidden="true">
          {repository.repo.slice(0, 1).toUpperCase()}
        </div>
        <div>
          <span>{repository.owner}</span>
          <h2>{repository.repo}</h2>
        </div>
        <WorkbenchStatus value={repository.readiness} />
      </div>
      <dl className="repository-checks">
        <div>
          <dt>Maintenance App</dt>
          <dd>{installationLabel(repository.maintenanceAppStatus)}</dd>
        </div>
        <div>
          <dt>Policy verifier</dt>
          <dd>{installationLabel(repository.verifierAppStatus)}</dd>
        </div>
        <div>
          <dt>Eligible issues</dt>
          <dd>{repository.eligibleIssueCount ?? 'Check repository'}</dd>
        </div>
      </dl>
      {repository.reason && <p className="workbench-card-reason">{repository.reason}</p>}
      {repository.readiness === 'unavailable' && retry ? (
        <button className="workbench-card-link" type="button" onClick={retry}>
          Retry readiness <span aria-hidden="true">↻</span>
        </button>
      ) : (
        <Link className="workbench-card-link" href={href}>
          {repository.readiness === 'ready' ? 'Open repository' : 'Review requirements'}
          <span aria-hidden="true">↗</span>
        </Link>
      )}
    </article>
  );
}

function installationLabel(status: InstallationStatus): string {
  if (status === 'installed') return 'Installed';
  if (status === 'missing') return 'Required';
  return 'Unavailable';
}

export function JobRow({ job }: { job: WorkbenchJob }) {
  const issueNumber = jobIssueNumber(job);
  return (
    <Link className="workbench-job-row" href={`/app/jobs/${encodeURIComponent(job.id)}`}>
      <div className="workbench-job-main">
        <span>
          {jobRepository(job)} {issueNumber ? `· issue #${issueNumber}` : ''}
        </span>
        <strong>{job.issueTitle || stateLabel(job.state)}</strong>
      </div>
      <div className="workbench-job-price">
        <strong>{formatUsdcAtomic(job.priceAtomic)}</strong>
        <small>{relativeTime(job.updatedAt)}</small>
      </div>
      <WorkbenchStatus value={job.state} />
      <span className="workbench-row-arrow" aria-hidden="true">
        ↗
      </span>
    </Link>
  );
}

export function ServiceContractNote() {
  return (
    <aside className="workbench-contract-note">
      <span aria-hidden="true">✓</span>
      <div>
        <strong>Validated pull request or refund of the quoted USDC payment</strong>
        <p>
          A separate policy signer verifies the payment and controls the refund path. Mizuki cannot
          move refund funds. Solana network and wallet fees are separate.
        </p>
      </div>
    </aside>
  );
}

export function LastChecked({ value }: { value?: string }) {
  if (!value) return null;
  return <span className="workbench-last-checked">Checked {formatTime(value)}</span>;
}
