'use client';

import Link from 'next/link';
import { useMemo, useState } from 'react';
import { JobReceipt } from '@/components/job-receipt';
import { formatTime, formatUsdcAtomic, stateLabel } from '@/lib/format';
import {
  isActiveJob,
  jobIssueNumber,
  jobRepository,
  normalizeJob,
  normalizeJobPage,
  type WorkbenchJob,
} from '@/lib/workbench';
import { useWorkbenchResource } from '@/lib/workbench-client';
import {
  JobRow,
  ServiceContractNote,
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

type JobFilter = 'all' | 'active' | 'delivered' | 'refunded';

export function Jobs() {
  const jobs = useWorkbenchResource('/v1/account/jobs', normalizeJobPage);
  const [filter, setFilter] = useState<JobFilter>('all');
  const visible = useMemo(() => {
    if (jobs.status !== 'ready') return [];
    if (filter === 'active') return jobs.data.jobs.filter(isActiveJob);
    if (filter === 'delivered') return jobs.data.jobs.filter((job) => job.state === 'delivered');
    if (filter === 'refunded') return jobs.data.jobs.filter((job) => job.state === 'refunded');
    return jobs.data.jobs;
  }, [filter, jobs]);

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Commercial work"
        title="Jobs"
        description="Every paid job resolves to a validated pull request or a full refund of the quoted USDC payment."
        action={<Link href="/app/jobs/new">New maintenance job</Link>}
      />

      <div className="workbench-filter-bar">
        <div aria-label="Filter jobs">
          {(['all', 'active', 'delivered', 'refunded'] as const).map((value) => (
            <button
              type="button"
              className={filter === value ? 'active' : ''}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
              key={value}
            >
              {value}
            </button>
          ))}
        </div>
        {jobs.status === 'ready' && (
          <span>
            {visible.length} {jobs.data.truncated ? 'shown' : 'jobs'}
          </span>
        )}
      </div>

      {jobs.status === 'ready' && jobs.data.truncated && (
        <p className="job-history-scope-note">
          Every job with payment, delivery, or refund work still in progress is included, plus the
          latest {jobs.data.limit?.toLocaleString() ?? 'available'} completed jobs. Filters apply to
          these records.
        </p>
      )}

      {jobs.status === 'loading' ? (
        <WorkbenchLoading label="Loading jobs" />
      ) : jobs.status === 'error' ? (
        <WorkbenchError
          title="Jobs could not be loaded"
          detail="Existing work continues. No payment action was repeated."
          retry={jobs.refresh}
        />
      ) : visible.length > 0 ? (
        <div className="workbench-panel workbench-job-list full-job-list">
          {visible.map((job) => (
            <JobRow job={job} key={job.id} />
          ))}
        </div>
      ) : jobs.status === 'ready' ? (
        <WorkbenchEmpty
          title={jobs.data.jobs.length === 0 ? 'No paid jobs yet' : `No ${filter} jobs`}
          detail={
            jobs.data.jobs.length === 0
              ? 'Choose one authorized issue in a ready repository to request a fixed quote.'
              : 'Change the filter to review the rest of your maintenance history.'
          }
          action={
            jobs.data.jobs.length === 0 ? <Link href="/app/jobs/new">Start a job</Link> : undefined
          }
        />
      ) : null}
    </div>
  );
}

export function JobRoom({ id }: { id: string }) {
  const job = useWorkbenchResource(`/v1/jobs/${encodeURIComponent(id)}`, normalizeJob);

  if (job.status === 'loading') {
    return (
      <div className="workbench-page">
        <WorkbenchLoading label="Loading job room" />
      </div>
    );
  }
  if (job.status === 'error') {
    return (
      <div className="workbench-page">
        <WorkbenchError
          title="This job could not be loaded"
          detail="The job continues independently. No payment action was repeated."
          retry={job.refresh}
        />
      </div>
    );
  }
  if (job.status !== 'ready') return null;

  const issueNumber = jobIssueNumber(job.data);
  const failed = ['rejected', 'failed', 'refund_pending', 'refunded'].includes(job.data.state);

  return (
    <div className="workbench-page workbench-job-room">
      <header className="job-room-header">
        <div>
          <p>
            {jobRepository(job.data)} {issueNumber ? `· issue #${issueNumber}` : ''}
          </p>
          <h1>{job.data.issueTitle || stateLabel(job.data.state)}</h1>
          <span>Created {formatTime(job.data.createdAt)}</span>
        </div>
        <div className="job-room-header-meta">
          <WorkbenchStatus value={job.data.state} />
          <strong>{formatUsdcAtomic(job.data.priceAtomic)}</strong>
        </div>
      </header>

      <div className={`job-room-outcome ${failed ? 'failed' : ''}`}>
        <div>
          <span>{failed ? 'Protected failure path' : 'Current job outcome'}</span>
          <strong>
            {job.data.state === 'delivered'
              ? 'Validated pull request opened'
              : job.data.state === 'refunded'
                ? 'Full refund finalized'
                : failed
                  ? 'Delivery stopped; refund protection remains active'
                  : 'Work is progressing toward a validated pull request'}
          </strong>
        </div>
        <Link href={`/jobs/${encodeURIComponent(job.data.id)}`}>View public receipt ↗</Link>
      </div>

      <JobReceipt initial={job.data} live />
      <ServiceContractNote />
    </div>
  );
}
