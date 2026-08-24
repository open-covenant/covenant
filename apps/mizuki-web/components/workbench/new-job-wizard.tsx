'use client';

import type { SolanaSignTransactionFeature } from '@solana/wallet-standard-features';
import Link from 'next/link';
import { useEffect, useMemo, useState } from 'react';
import { useRouter } from 'next/navigation';
import { formatTime, formatUsdcAtomic, stateLabel } from '@/lib/format';
import { githubIssuePattern } from '@/lib/github-url';
import { paymentKey, quoteExpired, quoteMatchesIssue, readJsonResponse } from '@/lib/payment';
import type { Job, Quote } from '@/lib/types';
import {
  normalizeIssues,
  normalizePreflight,
  normalizeRepositories,
  type WorkbenchIssue,
  type WorkbenchPreflight,
  type WorkbenchRepository,
} from '@/lib/workbench';
import {
  useWorkbenchResource,
  workbenchRequest,
  WorkbenchRequestError,
} from '@/lib/workbench-client';
import { useStandardWallet } from '@/lib/wallet-standard';
import { createPaymentFetch } from '@/lib/x402';
import {
  ServiceContractNote,
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';

export function NewJobWizard({
  initialOwner,
  initialRepo,
  initialIssue,
}: {
  initialOwner?: string;
  initialRepo?: string;
  initialIssue?: number;
}) {
  const repositories = useWorkbenchResource('/v1/account/repositories', normalizeRepositories);
  const [selected, setSelected] = useState<string>(
    initialOwner && initialRepo ? `${initialOwner}/${initialRepo}` : '',
  );
  const repository =
    repositories.status === 'ready'
      ? repositories.data.find((item) => item.fullName.toLowerCase() === selected.toLowerCase())
      : undefined;

  return (
    <div className="workbench-page narrow-workbench-page">
      <WorkbenchPageHeader
        eyebrow="New maintenance job"
        title="Choose one authorized issue"
        description="Review repository eligibility, receive an exact USDC quote, and pay only when the contract is clear."
      />

      <WizardProgress repository={repository} />

      {repositories.status === 'loading' ? (
        <WorkbenchLoading label="Loading connected repositories" />
      ) : repositories.status === 'error' ? (
        <WorkbenchError
          title="Repositories could not be loaded"
          detail="No quote or payment was created."
          retry={repositories.refresh}
        />
      ) : repositories.status === 'ready' && repositories.data.length === 0 ? (
        <WorkbenchEmpty
          title="Connect a repository first"
          detail="Both required GitHub Apps must be installed on the same public repository."
          action={<Link href="/app/onboarding">Connect repository</Link>}
        />
      ) : repositories.status === 'ready' ? (
        <>
          <section className="wizard-step">
            <div className="wizard-step-number">01</div>
            <div className="wizard-step-content">
              <div className="wizard-step-heading">
                <div>
                  <span>Repository</span>
                  <h2>Choose a ready repository</h2>
                </div>
                {repository && <WorkbenchStatus value={repository.readiness} />}
              </div>
              <div className="wizard-repository-grid">
                {repositories.data.map((item) => (
                  <button
                    type="button"
                    className={selected === item.fullName ? 'selected' : ''}
                    onClick={() => setSelected(item.fullName)}
                    disabled={item.readiness !== 'ready'}
                    key={item.fullName}
                  >
                    <span>{item.owner}</span>
                    <strong>{item.repo}</strong>
                    <small>
                      {item.readiness === 'ready' ? 'Ready for work' : 'Setup required'}
                    </small>
                  </button>
                ))}
              </div>
              {repository?.readiness !== 'ready' && selected && (
                <p className="wizard-help">
                  Finish the repository setup before requesting a quote.{' '}
                  <Link href={`/app/repositories/${repository?.owner}/${repository?.repo}`}>
                    Review requirements
                  </Link>
                </p>
              )}
            </div>
          </section>

          {repository?.readiness === 'ready' && (
            <IssueAndPayment repository={repository} initialIssue={initialIssue} />
          )}
        </>
      ) : null}
      <ServiceContractNote />
    </div>
  );
}

function IssueAndPayment({
  repository,
  initialIssue,
}: {
  repository: WorkbenchRepository;
  initialIssue?: number;
}) {
  const router = useRouter();
  const issues = useWorkbenchResource(
    `/v1/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}/issues`,
    normalizeIssues,
  );
  const [issueUrl, setIssueUrl] = useState('');
  const [preflight, setPreflight] = useState<WorkbenchPreflight | null>(null);
  const [quote, setQuote] = useState<Quote | null>(null);
  const [state, setState] = useState<'idle' | 'checking' | 'quoting' | 'quoted' | 'paying'>('idle');
  const [error, setError] = useState<string | null>(null);
  const {
    wallets,
    connected,
    connecting,
    error: walletError,
    connect,
  } = useStandardWallet('transaction');

  useEffect(() => {
    if (issues.status !== 'ready' || issueUrl) return;
    const initial = issues.data.find((item) => item.number === initialIssue);
    if (initial) setIssueUrl(initial.url);
  }, [initialIssue, issueUrl, issues]);

  useEffect(() => {
    setIssueUrl('');
    setPreflight(null);
    setQuote(null);
    setError(null);
    setState('idle');
  }, [repository.fullName]);

  const selectedIssue =
    issues.status === 'ready' ? issues.data.find((item) => item.url === issueUrl) : undefined;
  const issueSelectionLocked = state === 'checking' || state === 'quoting' || state === 'paying';

  function selectIssue(next: string) {
    setIssueUrl(next);
    setPreflight(null);
    setQuote(null);
    setError(null);
    setState('idle');
  }

  async function runPreflight(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setState('checking');
    setError(null);
    setPreflight(null);
    setQuote(null);
    try {
      const value = await workbenchRequest<unknown>('/v1/preflights', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: issueUrl.trim() }),
      });
      const next = normalizePreflight(value);
      setPreflight(next);
      setQuote(next.quote ?? null);
      setState(next.quote ? 'quoted' : 'idle');
    } catch (cause) {
      setState('idle');
      setError(preflightError(cause));
    }
  }

  async function requestQuote() {
    setState('quoting');
    setError(null);
    try {
      const next = await workbenchRequest<Quote>('/v1/quotes', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: issueUrl.trim() }),
      });
      setQuote(next);
      setState('quoted');
    } catch (cause) {
      setState('idle');
      setError(quoteError(cause));
    }
  }

  async function payAndStart() {
    if (!quote || !connected) return;
    if (!quoteMatchesIssue(quote, issueUrl)) {
      setQuote(null);
      setState('idle');
      setError('The selected issue changed. Review its scope and request a new fixed quote.');
      return;
    }
    if (quoteExpired(quote)) {
      setQuote(null);
      setState('idle');
      setError('This quote expired. Request a new fixed quote before paying.');
      return;
    }
    setState('paying');
    setError(null);
    try {
      const feature = connected.wallet.features[
        'solana:signTransaction'
      ] as SolanaSignTransactionFeature['solana:signTransaction'];
      const paidFetch = createPaymentFetch({
        account: connected.account,
        feature,
        quotePayment: quote.payment,
        quoteAmount: quote.priceAtomic,
      });
      const response = await paidFetch('/api/mizuki/v1/jobs', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': paymentKey(quote.id),
        },
        body: JSON.stringify({ quote_id: quote.id }),
      });
      const job = await readJsonResponse<Job>(response);
      router.push(`/app/jobs/${encodeURIComponent(job.id)}`);
    } catch {
      setState('quoted');
      setError(
        `Payment status could not be confirmed. Do not submit another payment. Check your wallet activity, then use quote ${quote.id} when contacting support.`,
      );
    }
  }

  return (
    <>
      <section className="wizard-step">
        <div className="wizard-step-number">02</div>
        <div className="wizard-step-content">
          <div className="wizard-step-heading">
            <div>
              <span>Authorized issue</span>
              <h2>Choose one clearly scoped issue</h2>
            </div>
            {selectedIssue && <WorkbenchStatus value={selectedIssue.eligibility} />}
          </div>

          {issues.status === 'loading' ? (
            <WorkbenchLoading label="Loading eligible issues" />
          ) : issues.status === 'error' ? (
            <WorkbenchError
              title="Issues could not be checked"
              detail="No quote, branch, or payment was created."
              retry={issues.refresh}
            />
          ) : (
            <form className="wizard-issue-form" onSubmit={runPreflight}>
              {issues.status === 'ready' && issues.data.length > 0 && (
                <div className="wizard-issue-options">
                  {issues.data.map((issue) => (
                    <IssueOption
                      issue={issue}
                      selected={issueUrl === issue.url}
                      disabled={issueSelectionLocked}
                      select={() => selectIssue(issue.url)}
                      key={issue.number}
                    />
                  ))}
                </div>
              )}
              <label htmlFor="workbench-issue-url">
                Or paste a public GitHub issue URL
                <input
                  id="workbench-issue-url"
                  type="url"
                  inputMode="url"
                  value={issueUrl}
                  onChange={(event) => selectIssue(event.target.value)}
                  disabled={issueSelectionLocked}
                  placeholder="https://github.com/owner/repository/issues/123"
                  pattern={githubIssuePattern}
                  required
                />
              </label>
              <button type="submit" disabled={state === 'checking' || !issueUrl}>
                {state === 'checking' ? 'Checking scope…' : 'Review maintenance scope'}
              </button>
            </form>
          )}
        </div>
      </section>

      {preflight && (
        <section className="wizard-step">
          <div className="wizard-step-number">03</div>
          <div className="wizard-step-content">
            <div className="wizard-step-heading">
              <div>
                <span>Preflight</span>
                <h2>
                  {preflight.eligibility === 'ready'
                    ? 'Ready for a fixed quote'
                    : preflight.eligibility === 'unsupported'
                      ? 'This issue is not supported'
                      : 'Issue changes required'}
                </h2>
              </div>
              <WorkbenchStatus value={preflight.eligibility} />
            </div>
            <dl className="preflight-grid">
              <div>
                <dt>Repository</dt>
                <dd>{preflight.repository}</dd>
              </div>
              <div>
                <dt>Scope</dt>
                <dd>{preflight.class ? stateLabel(preflight.class) : 'Not eligible'}</dd>
              </div>
              <div>
                <dt>Maximum files</dt>
                <dd>{preflight.maxFiles ?? 'Confirmed with quote'}</dd>
              </div>
              <div>
                <dt>Repository checks</dt>
                <dd>{preflight.validationCommands.join(' · ') || 'Confirmed before payment'}</dd>
              </div>
            </dl>
            {preflight.reason && <p className="preflight-reason">{preflight.reason}</p>}
            {preflight.eligibility === 'ready' && !quote && (
              <button
                type="button"
                onClick={() => void requestQuote()}
                disabled={state === 'quoting'}
              >
                {state === 'quoting' ? 'Creating quote…' : 'Get fixed quote'}
              </button>
            )}
            {preflight.eligibility !== 'ready' && (
              <a href={preflight.issue.url} target="_blank" rel="noreferrer">
                Update issue on GitHub ↗
              </a>
            )}
          </div>
        </section>
      )}

      {quote && (
        <>
          <section className="wizard-step">
            <div className="wizard-step-number">04</div>
            <div className="wizard-step-content">
              <div className="wizard-contract-heading">
                <div>
                  <span>Fixed-price maintenance contract</span>
                  <strong>{formatUsdcAtomic(quote.priceAtomic)}</strong>
                </div>
                <span>{stateLabel(quote.class)}</span>
              </div>
              <h2>{quote.issueTitle}</h2>
              <dl className="preflight-grid">
                <div>
                  <dt>Authorized issue</dt>
                  <dd>
                    <a href={quote.issueUrl} target="_blank" rel="noreferrer">
                      {quote.owner}/{quote.repo}#{quote.issueNumber} ↗
                    </a>
                  </dd>
                </div>
                <div>
                  <dt>Maximum files</dt>
                  <dd>{quote.maxFiles}</dd>
                </div>
                <div>
                  <dt>Quote valid until</dt>
                  <dd>{formatTime(quote.expiresAt)}</dd>
                </div>
                <div>
                  <dt>Delivery</dt>
                  <dd>Validated pull request</dd>
                </div>
                <div>
                  <dt>If delivery fails</dt>
                  <dd>Full refund of the quoted USDC payment</dd>
                </div>
              </dl>
              <p className="wizard-contract-copy">
                A separate policy signer verifies the payment and controls the refund path. Mizuki
                cannot move refund funds. Solana network and wallet fees are separate and are not
                refunded.
              </p>
            </div>
          </section>

          <section className="wizard-step">
            <div className="wizard-step-number">05</div>
            <div className="wizard-step-content">
              <div className="wizard-step-heading">
                <div>
                  <span>Payment</span>
                  <h2>Pay the exact quote and start</h2>
                </div>
              </div>
              {!connected ? (
                wallets.length > 0 ? (
                  <div className="workbench-wallet-options">
                    {wallets.map((wallet) => (
                      <button
                        type="button"
                        key={wallet.name}
                        disabled={Boolean(connecting)}
                        onClick={() => void connect(wallet)}
                      >
                        <span>{wallet.name}</span>
                        <strong>{connecting === wallet.name ? 'Connecting…' : 'Connect ↗'}</strong>
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="workbench-wallet-missing">
                    <strong>No compatible Solana wallet detected</strong>
                    <p>Open this page in a browser with a Wallet Standard-compatible wallet.</p>
                  </div>
                )
              ) : (
                <button
                  className="wizard-pay-button"
                  type="button"
                  disabled={state === 'paying'}
                  onClick={() => void payAndStart()}
                >
                  {state === 'paying'
                    ? 'Confirming payment…'
                    : `Pay ${formatUsdcAtomic(quote.priceAtomic)} and start`}
                </button>
              )}
              <p className="wizard-consent">
                By paying, you accept the <Link href="/terms">service terms</Link> and acknowledge
                the <Link href="/privacy">privacy notice</Link>.
              </p>
            </div>
          </section>
        </>
      )}

      {(error || walletError) && (
        <div className="workbench-form-error" role="alert">
          <strong>Action required</strong>
          <p>{error || walletError}</p>
        </div>
      )}
    </>
  );
}

function IssueOption({
  issue,
  selected,
  disabled,
  select,
}: {
  issue: WorkbenchIssue;
  selected: boolean;
  disabled: boolean;
  select: () => void;
}) {
  return (
    <button
      type="button"
      className={selected ? 'selected' : ''}
      onClick={select}
      disabled={disabled}
    >
      <span>Issue #{issue.number}</span>
      <strong>{issue.title}</strong>
      <small>
        {issue.authorized
          ? issue.reason || 'Maintainer authorization confirmed'
          : 'Authorization label required'}
      </small>
    </button>
  );
}

function WizardProgress({ repository }: { repository?: WorkbenchRepository }) {
  return (
    <ol className="wizard-progress" aria-label="New job progress">
      {['Repository', 'Issue', 'Preflight', 'Contract', 'Pay'].map((label, index) => (
        <li className={index === 0 && repository ? 'complete' : ''} key={label}>
          <span>{String(index + 1).padStart(2, '0')}</span>
          {label}
        </li>
      ))}
    </ol>
  );
}

function preflightError(cause: unknown): string {
  if (cause instanceof WorkbenchRequestError) {
    if (cause.status === 409) return 'The issue or repository changed. Check the scope again.';
    if (cause.status === 422) return cause.message;
    if (cause.status === 429) return 'Too many checks were requested. Wait a moment and try again.';
  }
  return 'The issue could not be checked. No quote, branch, or payment was created.';
}

function quoteError(cause: unknown): string {
  if (cause instanceof WorkbenchRequestError) {
    if (cause.status === 409) return 'The issue or repository changed. Run preflight again.';
    if (cause.status === 422) return cause.message;
    if (cause.status === 429) return 'Too many quote requests. Wait a moment and try again.';
  }
  return 'A fixed quote could not be created. No payment was requested.';
}
