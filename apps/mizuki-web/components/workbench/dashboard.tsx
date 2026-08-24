'use client';

import Link from 'next/link';
import { formatUsdcAtomic } from '@/lib/format';
import {
  isActiveJob,
  normalizeJobs,
  normalizeRepositories,
  type WorkbenchJob,
} from '@/lib/workbench';
import { useWorkbenchResource } from '@/lib/workbench-client';
import {
  JobRow,
  RepositoryCard,
  ServiceContractNote,
  SummaryCard,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
} from './workbench-primitives';

export function Dashboard() {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);
  const jobs = useWorkbenchResource('/v1/account/jobs', normalizeJobs);

  if (repositories.status === 'loading' || jobs.status === 'loading') {
    return (
      <WorkbenchPage>
        <WorkbenchLoading label="Loading maintenance work" />
      </WorkbenchPage>
    );
  }
  if (repositories.status === 'error' || jobs.status === 'error') {
    return (
      <WorkbenchPage>
        <WorkbenchError
          title="Maintenance work is temporarily unavailable"
          detail="No payment or repository action was attempted."
          retry={() => {
            repositories.refresh();
            jobs.refresh();
          }}
        />
      </WorkbenchPage>
    );
  }
  if (repositories.status !== 'ready' || jobs.status !== 'ready') return null;

  const active = jobs.data.filter(isActiveJob);
  const recent = [...jobs.data]
    .sort((left, right) => Date.parse(right.updatedAt) - Date.parse(left.updatedAt))
    .slice(0, 5);
  const readyRepositories = repositories.data.filter((item) => item.readiness === 'ready');
  const actionRequired = repositories.data.filter((item) => item.readiness === 'action_required');
  const finalizedRefunds = jobs.data.filter((item) => item.state === 'refunded').length;
  const totalPaid = paidTotal(jobs.data);

  return (
    <WorkbenchPage>
      <WorkbenchPageHeader
        eyebrow="Mizuki Workbench"
        title="Maintenance work"
        description="Track authorized repositories, paid jobs, pull requests, and refunds."
        action={<Link href="/app/jobs/new">New maintenance job</Link>}
      />

      {repositories.data.length === 0 ? (
        <FirstRepository />
      ) : actionRequired.length > 0 ? (
        <div className="workbench-attention-bar">
          <div>
            <strong>{actionRequired.length} repositories need attention</strong>
            <p>Finish the GitHub App setup before requesting paid work.</p>
          </div>
          <Link href="/app/repositories">Review repositories</Link>
        </div>
      ) : null}

      <div className="workbench-summary-grid">
        <SummaryCard
          label="Active jobs"
          value={active.length}
          detail="Paid work still in progress"
        />
        <SummaryCard
          label="Ready repositories"
          value={readyRepositories.length}
          detail="Both GitHub Apps verified"
        />
        <SummaryCard
          label="Paid"
          value={formatUsdcAtomic(totalPaid)}
          detail="Across the jobs visible to this account"
        />
        <SummaryCard
          label="Refunds finalized"
          value={finalizedRefunds}
          detail="Returned to original payer wallets"
        />
      </div>

      <div className="workbench-dashboard-grid">
        <section className="workbench-panel workbench-panel-large">
          <div className="workbench-panel-heading">
            <div>
              <span>Work queue</span>
              <h2>{active.length > 0 ? 'Jobs in progress' : 'Recent jobs'}</h2>
            </div>
            <Link href="/app/jobs">View all</Link>
          </div>
          {recent.length > 0 ? (
            <div className="workbench-job-list">
              {(active.length > 0 ? active.slice(0, 5) : recent).map((job) => (
                <JobRow job={job} key={job.id} />
              ))}
            </div>
          ) : (
            <div className="workbench-inline-empty">
              <strong>No paid jobs yet</strong>
              <p>Choose a ready repository and one authorized issue to request a fixed quote.</p>
              <Link href="/app/jobs/new">Start a job</Link>
            </div>
          )}
        </section>

        <aside className="workbench-panel">
          <div className="workbench-panel-heading">
            <div>
              <span>Repository readiness</span>
              <h2>Ready for work</h2>
            </div>
            <Link href="/app/repositories">Manage</Link>
          </div>
          {readyRepositories.length > 0 ? (
            <div className="dashboard-repositories">
              {readyRepositories.slice(0, 3).map((repository) => (
                <RepositoryCard repository={repository} key={repository.fullName} />
              ))}
            </div>
          ) : (
            <div className="workbench-inline-empty">
              <strong>No ready repositories</strong>
              <p>Both required GitHub Apps must be installed on the same public repository.</p>
              <Link href="/app/onboarding">Finish setup</Link>
            </div>
          )}
        </aside>
      </div>

      <ServiceContractNote />
    </WorkbenchPage>
  );
}

function WorkbenchPage({ children }: { children: React.ReactNode }) {
  return <div className="workbench-page">{children}</div>;
}

function FirstRepository() {
  return (
    <section className="workbench-first-run">
      <div>
        <span>Start here</span>
        <h2>Connect one public repository</h2>
        <p>
          Install the maintenance App and separate policy verifier on the same repository. Mizuki
          can then verify its supported issues before you request a quote.
        </p>
      </div>
      <Link href="/app/onboarding">Connect repository</Link>
    </section>
  );
}

function paidTotal(jobs: WorkbenchJob[]): string {
  return jobs
    .filter((job) => !['quoted', 'settlement_pending'].includes(job.state))
    .reduce((total, job) => total + BigInt(job.priceAtomic || '0'), 0n)
    .toString();
}
