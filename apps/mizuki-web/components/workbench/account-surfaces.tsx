'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { truncateAddress } from '@/lib/format';
import { normalizeAccount, normalizeRepositories } from '@/lib/workbench';
import { useWorkbenchResource, workbenchRequest } from '@/lib/workbench-client';
import {
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

const maintenanceAppUrl = 'https://github.com/apps/mizuki-the-mech-core/installations/new';
const verifierAppUrl = 'https://github.com/apps/mizuki-the-mech-policy-verifier/installations/new';

export function Integrations() {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Connected services"
        title="Integrations"
        description="Review the GitHub access required for repository maintenance and the payment tools used per job."
      />

      <section className="workbench-panel integration-panel">
        <div className="integration-heading">
          <div className="integration-mark">GH</div>
          <div>
            <span>GitHub</span>
            <h2>Repository access</h2>
            <p>Both Apps must be installed on the exact public repository that receives work.</p>
          </div>
        </div>
        <div className="integration-actions">
          <a href={maintenanceAppUrl} target="_blank" rel="noreferrer">
            Install maintenance App ↗
          </a>
          <a href={verifierAppUrl} target="_blank" rel="noreferrer">
            Install policy verifier ↗
          </a>
        </div>
        {repositories.status === 'loading' ? (
          <WorkbenchLoading label="Loading GitHub installations" />
        ) : repositories.status === 'error' ? (
          <WorkbenchError
            title="GitHub installations could not be confirmed"
            retry={repositories.refresh}
          />
        ) : repositories.status === 'ready' ? (
          <div className="integration-repositories">
            {repositories.data.length > 0 ? (
              repositories.data.map((repository) => (
                <Link
                  href={`/app/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}`}
                  key={repository.fullName}
                >
                  <span>{repository.fullName}</span>
                  <WorkbenchStatus value={repository.readiness} />
                </Link>
              ))
            ) : (
              <p>No repositories are connected to this GitHub account.</p>
            )}
          </div>
        ) : null}
      </section>

      <div className="integration-grid">
        <section className="workbench-panel integration-panel compact-integration-panel">
          <div className="integration-heading">
            <div className="integration-mark">◎</div>
            <div>
              <span>Solana</span>
              <h2>Pay per job</h2>
              <p>
                Connect a compatible wallet only when paying a fixed quote or proving a bounty
                payout address.
              </p>
            </div>
          </div>
          <Link href="/app/billing">View payments & refunds</Link>
        </section>

        <section className="workbench-panel integration-panel compact-integration-panel">
          <div className="integration-heading">
            <div className="integration-mark">API</div>
            <div>
              <span>Machine access</span>
              <h2>No Workbench credentials</h2>
              <p>
                Workbench does not issue customer API keys or webhooks in this release. No hidden
                credential has been created for this account.
              </p>
            </div>
          </div>
          <a
            href="https://clawpump.tech/marketplace/agents/711fa8b1-5f37-4451-b7a7-bfcb9a021f6d"
            target="_blank"
            rel="noreferrer"
          >
            View ClawPump listing ↗
          </a>
        </section>
      </div>
    </div>
  );
}

export function Settings() {
  const router = useRouter();
  const account = useWorkbenchResource('/v1/account', normalizeAccount);

  async function logout() {
    await workbenchRequest('/v1/auth/logout', { method: 'POST' }).catch(() => undefined);
    router.push('/');
    router.refresh();
  }

  return (
    <div className="workbench-page narrow-workbench-page">
      <WorkbenchPageHeader
        eyebrow="Account"
        title="Settings"
        description="Review the GitHub identity and wallet information attached to your Workbench session."
      />
      <MobileMoreNavigation />
      {account.status === 'loading' ? (
        <WorkbenchLoading label="Loading account settings" />
      ) : account.status === 'error' ? (
        <WorkbenchError title="Account settings could not be loaded" retry={account.refresh} />
      ) : account.status === 'ready' ? (
        <>
          <section className="workbench-panel settings-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>GitHub identity</span>
                <h2>@{account.data.githubLogin}</h2>
              </div>
              <WorkbenchStatus value="ready" />
            </div>
            <p>
              This identity controls which connected repositories and contributor claims appear in
              Workbench.
            </p>
          </section>

          <section className="workbench-panel settings-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>Verified payout wallet</span>
                <h2>
                  {account.data.walletAddress
                    ? truncateAddress(account.data.walletAddress, 8)
                    : 'No payout wallet verified'}
                </h2>
              </div>
            </div>
            <p>
              A payout wallet is verified only when you sign a free ownership challenge while
              claiming a funded bounty. Job refunds always return to the wallet that paid.
            </p>
          </section>

          <section className="workbench-panel settings-panel">
            <div className="workbench-panel-heading">
              <div>
                <span>Notifications</span>
                <h2>Job-room updates only</h2>
              </div>
            </div>
            <p>
              Email and webhook notifications are not enabled in this release. Live status remains
              available inside each job room and on its public receipt.
            </p>
          </section>

          <section className="workbench-settings-actions">
            <button type="button" onClick={() => void logout()}>
              Sign out of Workbench
            </button>
            <div>
              <Link href="/privacy">Privacy</Link>
              <Link href="/terms">Terms</Link>
              <Link href="/support">Support</Link>
            </div>
          </section>
        </>
      ) : null}
    </div>
  );
}

export function MobileMoreNavigation() {
  return (
    <nav className="workbench-mobile-more" aria-label="More Workbench sections">
      <Link href="/app/billing">Payments & refunds</Link>
      <Link href="/app/integrations">Integrations</Link>
      <Link href="/app/settings" aria-current="page">
        Settings
      </Link>
    </nav>
  );
}
