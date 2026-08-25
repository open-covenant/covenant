'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { useState } from 'react';
import { formatTime } from '@/lib/format';
import { normalizeIssues, normalizeRepositories, parseRepositoryLocator } from '@/lib/workbench';
import type { InstallationStatus } from '@/lib/workbench';
import { useWorkbenchResource, workbenchMutation } from '@/lib/workbench-client';
import {
  LastChecked,
  RepositoryCard,
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

const maintenanceAppUrl = 'https://github.com/apps/mizuki-the-mech-core/installations/new';
const verifierAppUrl = 'https://github.com/apps/mizuki-the-mech-policy-verifier/installations/new';

export function Repositories() {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Authorized scope"
        title="Repositories"
        description="See exactly which public repositories are ready for fixed-price maintenance."
        action={<Link href="/app/onboarding">Connect repository</Link>}
      />
      {repositories.status === 'loading' ? (
        <WorkbenchLoading label="Loading repositories" />
      ) : repositories.status === 'error' ? (
        <WorkbenchError
          title="Repositories could not be loaded"
          detail="Your GitHub installations were not changed."
          retry={repositories.refresh}
        />
      ) : repositories.status === 'ready' && repositories.data.length > 0 ? (
        <div className="workbench-repository-grid">
          {repositories.data.map((repository) => (
            <RepositoryCard
              repository={repository}
              retry={repositories.refresh}
              key={repository.fullName}
            />
          ))}
        </div>
      ) : repositories.status === 'ready' ? (
        <WorkbenchEmpty
          title="No repositories connected"
          detail="Install both required GitHub Apps on the same public repository, then check again."
          action={<Link href="/app/onboarding">Connect repository</Link>}
        />
      ) : null}
    </div>
  );
}

export function RepositoryWorkspace({ owner, repo }: { owner: string; repo: string }) {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);
  const issues = useWorkbenchResource(
    `/v1/repositories/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/issues`,
    normalizeIssues,
  );
  const repository =
    repositories.status === 'ready'
      ? repositories.data.find(
          (item) =>
            item.owner.toLowerCase() === owner.toLowerCase() &&
            item.repo.toLowerCase() === repo.toLowerCase(),
        )
      : undefined;

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow={owner}
        title={repo}
        description="Installation status, supported checks, eligible issues, and paid maintenance history."
        action={
          repository?.readiness === 'ready' ? (
            <Link
              href={`/app/jobs/new?owner=${encodeURIComponent(owner)}&repo=${encodeURIComponent(repo)}`}
            >
              Start a job
            </Link>
          ) : repository?.readiness === 'unavailable' ? (
            <button type="button" onClick={repositories.refresh}>
              Retry status
            </button>
          ) : undefined
        }
      />

      {repositories.status === 'loading' ? (
        <WorkbenchLoading label="Checking repository" />
      ) : repositories.status === 'error' ? (
        <WorkbenchError
          title="Repository status could not be confirmed"
          retry={repositories.refresh}
        />
      ) : repositories.status === 'ready' && !repository ? (
        <WorkbenchError
          title="This repository is not connected to your account"
          detail="Choose a repository available through your GitHub installations."
        />
      ) : repository ? (
        <>
          <section className="repository-readiness-panel">
            <div className="repository-readiness-heading">
              <div>
                <span>Repository readiness</span>
                <h2>
                  {repository.readiness === 'ready'
                    ? 'Ready for paid work'
                    : repository.readiness === 'unavailable'
                      ? 'Readiness temporarily unavailable'
                      : 'Setup required'}
                </h2>
              </div>
              <WorkbenchStatus value={repository.readiness} />
            </div>
            <div className="repository-readiness-checks">
              <ReadinessCheck
                label="Maintenance App"
                status={repository.maintenanceAppStatus}
                actionUrl={maintenanceAppUrl}
                retry={repositories.refresh}
              />
              <ReadinessCheck
                label="Policy verifier"
                status={repository.verifierAppStatus}
                actionUrl={verifierAppUrl}
                retry={repositories.refresh}
              />
              <ReadinessCheck
                label="Repository checks"
                status={
                  repository.readiness === 'ready'
                    ? 'installed'
                    : repository.readiness === 'unavailable'
                      ? 'unavailable'
                      : 'missing'
                }
                detail={
                  repository.validationCommands.join(' · ') ||
                  (repository.readiness === 'ready'
                    ? 'Confirmed for each issue during preflight'
                    : repository.readiness === 'unavailable'
                      ? 'Status could not be confirmed. Retry the readiness check.'
                      : 'Confirmed after both Apps are installed')
                }
                retry={repositories.refresh}
              />
            </div>
            {repository.reason && (
              <p className="repository-readiness-reason">{repository.reason}</p>
            )}
            <LastChecked value={repository.lastCheckedAt} />
          </section>

          <section className="workbench-panel repository-issues-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>Authorized scope</span>
                <h2>Repository issues</h2>
              </div>
              <button type="button" onClick={issues.refresh}>
                Check again
              </button>
            </div>
            {issues.status === 'loading' ? (
              <WorkbenchLoading label="Loading repository issues" />
            ) : issues.status === 'error' ? (
              <WorkbenchError
                title="Issues could not be checked"
                detail="No issue, branch, or payment was changed."
                retry={issues.refresh}
              />
            ) : issues.status === 'ready' && issues.data.length > 0 ? (
              <div className="repository-issue-list">
                {issues.data.map((issue) => (
                  <article className="repository-issue" key={issue.number}>
                    <div>
                      <span>Issue #{issue.number}</span>
                      <h3>{issue.title}</h3>
                      <p>
                        {issue.reason ||
                          (issue.authorized
                            ? 'Maintainer authorization confirmed.'
                            : 'Add the mizuki:authorized label before requesting a quote.')}
                      </p>
                    </div>
                    <WorkbenchStatus value={issue.eligibility} />
                    <div className="repository-issue-actions">
                      <a href={issue.url} target="_blank" rel="noreferrer">
                        View issue
                      </a>
                      {issue.eligibility === 'ready' && (
                        <Link
                          href={`/app/jobs/new?owner=${encodeURIComponent(owner)}&repo=${encodeURIComponent(repo)}&issue=${issue.number}`}
                        >
                          Request quote
                        </Link>
                      )}
                    </div>
                  </article>
                ))}
              </div>
            ) : issues.status === 'ready' ? (
              <WorkbenchEmpty
                title="No issues are ready"
                detail="Add the mizuki:authorized label to one clearly scoped issue, then check again."
                action={
                  <a
                    href={`https://github.com/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/issues`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Open GitHub issues ↗
                  </a>
                }
              />
            ) : null}
          </section>
        </>
      ) : null}
    </div>
  );
}

export function RepositoryOnboarding() {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);
  const connected = repositories.status === 'ready' ? repositories.data : [];

  return (
    <div className="workbench-page narrow-workbench-page">
      <WorkbenchPageHeader
        eyebrow="Repository setup"
        title="Connect a public repository"
        description="Both GitHub Apps must be installed on the exact repository that will receive work."
      />
      <RepositoryConnector refresh={repositories.refresh} />
      <ol className="workbench-onboarding">
        <OnboardingStep
          number="01"
          title="Install the maintenance App"
          detail="It reads the authorized issue, creates one scoped branch, runs checks, and opens the pull request. It never merges."
          action={
            <a href={maintenanceAppUrl} target="_blank" rel="noreferrer">
              Install maintenance App ↗
            </a>
          }
        />
        <OnboardingStep
          number="02"
          title="Install the policy verifier"
          detail="It has read-only repository access and separately verifies authorization and delivery evidence."
          action={
            <a href={verifierAppUrl} target="_blank" rel="noreferrer">
              Install policy verifier ↗
            </a>
          }
        />
        <OnboardingStep
          number="03"
          title="Connect the repository"
          detail="Enter its GitHub URL above. Mizuki verifies both installations and links only repositories your GitHub account can maintain."
          action={
            <button type="button" onClick={repositories.refresh}>
              Refresh connected repositories
            </button>
          }
        />
      </ol>

      {repositories.status === 'loading' ? (
        <WorkbenchLoading label="Checking connected repositories" />
      ) : repositories.status === 'error' ? (
        <WorkbenchError
          title="Repository status could not be confirmed"
          retry={repositories.refresh}
        />
      ) : connected.length > 0 ? (
        <section className="workbench-panel onboarding-results">
          <div className="workbench-panel-heading">
            <div>
              <span>Connected repositories</span>
              <h2>{connected.length} connected repositories</h2>
            </div>
            <span>{formatTime(new Date().toISOString())}</span>
          </div>
          <div className="workbench-repository-grid">
            {connected.map((repository) => (
              <RepositoryCard
                repository={repository}
                retry={repositories.refresh}
                key={repository.fullName}
              />
            ))}
          </div>
        </section>
      ) : (
        <WorkbenchEmpty
          title="No connected repositories yet"
          detail="Complete both GitHub installation steps, then enter the repository URL above."
        />
      )}
    </div>
  );
}

function RepositoryConnector({ refresh }: { refresh: () => void }) {
  const router = useRouter();
  const [value, setValue] = useState('');
  const [state, setState] = useState<'idle' | 'checking'>('idle');
  const [error, setError] = useState<string | null>(null);

  async function connect(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const repository = parseRepositoryLocator(value);
    if (!repository) {
      setError('Enter a repository as owner/repository or https://github.com/owner/repository.');
      return;
    }
    setState('checking');
    setError(null);
    try {
      await workbenchMutation('/v1/account/repositories', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ owner: repository.owner, repo: repository.repo }),
      });
      refresh();
      router.push(
        `/app/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}`,
      );
    } catch (cause) {
      setError(
        cause instanceof Error
          ? cause.message
          : 'This repository could not be connected. Confirm both App installations and try again.',
      );
      setState('idle');
    }
  }

  return (
    <section className="repository-connector">
      <div>
        <span>Repository to connect</span>
        <h2>Verify one exact GitHub repository</h2>
        <p>Checking does not create a branch, change an issue, or request payment.</p>
      </div>
      <form onSubmit={connect}>
        <label htmlFor="repository-locator">GitHub repository URL or owner/repository</label>
        <div>
          <input
            id="repository-locator"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            placeholder="https://github.com/owner/repository"
            autoComplete="off"
            required
          />
          <button type="submit" disabled={state === 'checking'}>
            {state === 'checking' ? 'Verifying…' : 'Verify repository'}
          </button>
        </div>
      </form>
      {error && (
        <p className="workbench-form-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

export function ReadinessCheck({
  label,
  status,
  detail,
  actionUrl,
  retry,
}: {
  label: string;
  status: InstallationStatus;
  detail?: string;
  actionUrl?: string;
  retry?: () => void;
}) {
  const ready = status === 'installed';
  const unavailable = status === 'unavailable';
  return (
    <div className={ready ? 'ready' : unavailable ? 'unavailable' : ''}>
      <span aria-hidden="true">{ready ? '✓' : unavailable ? '…' : '!'}</span>
      <div>
        <strong>{label}</strong>
        <p>
          {detail ||
            (ready
              ? 'Verified on this repository'
              : unavailable
                ? 'Status could not be confirmed'
                : 'Required on this repository')}
        </p>
      </div>
      {status === 'missing' && actionUrl && (
        <a href={actionUrl} target="_blank" rel="noreferrer">
          Install ↗
        </a>
      )}
      {unavailable && retry && (
        <button type="button" onClick={retry}>
          Retry
        </button>
      )}
    </div>
  );
}

function OnboardingStep({
  number,
  title,
  detail,
  action,
}: {
  number: string;
  title: string;
  detail: string;
  action: React.ReactNode;
}) {
  return (
    <li>
      <span>{number}</span>
      <div>
        <h2>{title}</h2>
        <p>{detail}</p>
      </div>
      {action}
    </li>
  );
}
