'use client';

import Link from 'next/link';
import { useState } from 'react';
import { formatTime, truncateAddress } from '@/lib/format';
import {
  API_TOKEN_SCOPES,
  normalizeAccount,
  normalizeApiTokenCredential,
  normalizeApiTokens,
  normalizeRepositories,
  type ApiTokenCredential,
  type ApiTokenScope,
} from '@/lib/workbench';
import { logoutWorkbench, useWorkbenchResource, workbenchMutation } from '@/lib/workbench-client';
import {
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';
import { WorkbenchSelect } from './workbench-select';

const maintenanceAppUrl = 'https://github.com/apps/mizuki-the-mech-core/installations/new';
const verifierAppUrl = 'https://github.com/apps/mizuki-the-mech-policy-verifier/installations/new';

export function Integrations() {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);

  return (
    <div className="workbench-page">
      <WorkbenchPageHeader
        eyebrow="Connected services"
        title="Integrations"
        description="Manage repository access, payment connections, and scoped credentials for API clients."
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
              <span>Distribution</span>
              <h2>ClawPump</h2>
              <p>Open Mizuki's marketplace listing and review its public agent activity.</p>
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

      <MachineAccess />
    </div>
  );
}

export function MachineAccess() {
  const tokens = useWorkbenchResource('/v1/account/api-tokens', normalizeApiTokens);
  const [name, setName] = useState('MCP client');
  const [duration, setDuration] = useState(90);
  const [scopes, setScopes] = useState<ApiTokenScope[]>([...API_TOKEN_SCOPES]);
  const [credential, setCredential] = useState<ApiTokenCredential>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const [copied, setCopied] = useState(false);

  function toggleScope(scope: ApiTokenScope) {
    setScopes((current) =>
      current.includes(scope)
        ? current.filter((candidate) => candidate !== scope)
        : API_TOKEN_SCOPES.filter((candidate) => [...current, scope].includes(candidate)),
    );
  }

  async function create() {
    if (credential || scopes.length === 0 || !name.trim()) return;
    setPending(true);
    setError(undefined);
    try {
      const expiresAt = new Date(Date.now() + duration * 24 * 60 * 60_000).toISOString();
      const value = await workbenchMutation<unknown>('/v1/account/api-tokens', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name: name.trim(), scopes, expiresAt }),
      });
      setCredential(normalizeApiTokenCredential(value));
      tokens.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'The API token could not be created.');
    } finally {
      setPending(false);
    }
  }

  async function copySecret() {
    if (!credential) return;
    try {
      await navigator.clipboard.writeText(credential.secret);
      setCopied(true);
    } catch {
      setError('Copy failed. Select the token value and copy it manually.');
    }
  }

  async function revoke(id: string) {
    setPending(true);
    setError(undefined);
    try {
      await workbenchMutation(`/v1/account/api-tokens/${encodeURIComponent(id)}/revoke`, {
        method: 'POST',
      });
      tokens.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'The API token could not be revoked.');
    } finally {
      setPending(false);
    }
  }

  return (
    <section className="workbench-panel machine-access-panel">
      <div className="workbench-panel-heading">
        <div>
          <span>Machine access</span>
          <h2>Scoped API tokens</h2>
        </div>
        <WorkbenchStatus
          value={
            tokens.status === 'ready'
              ? 'ready'
              : tokens.status === 'error'
                ? 'unavailable'
                : 'checking'
          }
        />
      </div>
      <p className="machine-access-intro">
        Create an account-bound credential for another agent or API client. Each token is limited to
        the selected operations; it cannot install GitHub Apps, submit wallet signatures, or create
        another token.
      </p>
      <div className="machine-access-config">
        <span>HTTPS API</span>
        <code>Base URL: https://mizuki.opencovenant.org/api/mizuki</code>
        <code>Authorization: Bearer &lt;token copied after creation&gt;</code>
        <p>Keep the token in the client's secret storage. Do not commit it to a repository.</p>
      </div>

      {credential && (
        <div className="machine-token-secret" role="status">
          <div>
            <strong>Copy this token now</strong>
            <p>This value is shown once. Mizuki stores only its prefix and cryptographic hash.</p>
          </div>
          <code>{credential.secret}</code>
          <div className="machine-token-secret-actions">
            <button type="button" onClick={() => void copySecret()}>
              {copied ? 'Copied' : 'Copy token'}
            </button>
            <button
              type="button"
              onClick={() => {
                setCredential(undefined);
                setCopied(false);
              }}
            >
              I have stored it
            </button>
          </div>
        </div>
      )}

      <div className="machine-access-layout">
        <form
          className="machine-token-form"
          onSubmit={(event) => {
            event.preventDefault();
            void create();
          }}
        >
          <label>
            Token name
            <input
              value={name}
              maxLength={80}
              onChange={(event) => setName(event.target.value)}
              disabled={pending || Boolean(credential)}
            />
          </label>
          <div className="workbench-field">
            <span id="machine-token-expiration-label">Expiration</span>
            <WorkbenchSelect
              id="machine-token-expiration"
              labelledBy="machine-token-expiration-label"
              value={String(duration)}
              placeholder="Choose an expiration"
              options={[
                { value: '30', label: '30 days' },
                { value: '90', label: '90 days' },
                { value: '365', label: '1 year' },
              ]}
              disabled={pending || Boolean(credential)}
              onChange={(nextDuration) => setDuration(Number(nextDuration))}
            />
          </div>
          <fieldset disabled={pending || Boolean(credential)}>
            <legend>Scopes</legend>
            {API_TOKEN_SCOPES.map((scope) => (
              <label key={scope}>
                <input
                  type="checkbox"
                  checked={scopes.includes(scope)}
                  onChange={() => toggleScope(scope)}
                />
                <span>
                  <strong>{scope}</strong>
                  <small>{scopeDescription(scope)}</small>
                </span>
              </label>
            ))}
          </fieldset>
          <button
            type="submit"
            disabled={pending || Boolean(credential) || !name.trim() || scopes.length === 0}
          >
            {pending ? 'Creating…' : 'Create API token'}
          </button>
          {error && <p className="workbench-form-error">{error}</p>}
        </form>

        <div className="machine-token-records">
          <div>
            <strong>Active and recent tokens</strong>
            <p>All active tokens remain visible. Last-used times update after authentication.</p>
          </div>
          {tokens.status === 'loading' ? (
            <WorkbenchLoading label="Loading API tokens" />
          ) : tokens.status === 'error' ? (
            <WorkbenchError title="API tokens could not be loaded" retry={tokens.refresh} />
          ) : tokens.status === 'ready' && tokens.data.length > 0 ? (
            tokens.data.map((token) => (
              <article key={token.id}>
                <div>
                  <strong>{token.name}</strong>
                  <code>{token.prefix}…</code>
                </div>
                <WorkbenchStatus value={token.state} />
                <p>{token.scopes.join(' · ')}</p>
                <dl>
                  <div>
                    <dt>Expires</dt>
                    <dd>{formatTime(token.expiresAt)}</dd>
                  </div>
                  <div>
                    <dt>Last used</dt>
                    <dd>{token.lastUsedAt ? formatTime(token.lastUsedAt) : 'Never'}</dd>
                  </div>
                </dl>
                {token.state === 'active' && (
                  <button type="button" disabled={pending} onClick={() => void revoke(token.id)}>
                    Revoke
                  </button>
                )}
              </article>
            ))
          ) : (
            <p className="machine-token-empty">No API tokens have been issued.</p>
          )}
        </div>
      </div>
    </section>
  );
}

function scopeDescription(scope: ApiTokenScope): string {
  if (scope === 'repositories:read') return 'Repository readiness, eligible issues, and preflight';
  if (scope === 'jobs:read') return 'Quote payment recovery and reservation status';
  return 'Create account-linked maintenance quotes';
}

export function Settings() {
  const account = useWorkbenchResource('/v1/account', normalizeAccount);
  const [logoutPending, setLogoutPending] = useState(false);
  const [logoutError, setLogoutError] = useState<string>();

  async function logout() {
    setLogoutPending(true);
    setLogoutError(undefined);
    try {
      await logoutWorkbench(() => {
        window.location.replace('/app');
      });
    } catch {
      setLogoutError('Sign-out could not be confirmed. This page remains signed in; try again.');
    } finally {
      setLogoutPending(false);
    }
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
            <button type="button" onClick={() => void logout()} disabled={logoutPending}>
              {logoutPending ? 'Signing out…' : 'Sign out of Workbench'}
            </button>
            {logoutError && (
              <p className="workbench-logout-error" role="alert">
                {logoutError}
              </p>
            )}
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
