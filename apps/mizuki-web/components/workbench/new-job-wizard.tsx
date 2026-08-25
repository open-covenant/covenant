'use client';

import type { SolanaSignTransactionFeature } from '@solana/wallet-standard-features';
import Link from 'next/link';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useRouter } from 'next/navigation';
import { formatTime, formatUsdcAtomic, stateLabel, truncateAddress } from '@/lib/format';
import { githubIssuePattern } from '@/lib/github-url';
import {
  checkQuotePaymentStatus,
  clearWorkbenchPaymentRecovery,
  issueMatchesRepository,
  loadWorkbenchPaymentRecovery,
  paymentAccountId,
  PaymentRecoveryStorageError,
  prepareWorkbenchPaymentRecovery,
  quoteExpired,
  quoteMatchesIssue,
  readJsonResponse,
  saveWorkbenchPaymentRecovery,
  type WorkbenchPaymentRecovery,
} from '@/lib/payment';
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
  workbenchMutation,
  workbenchRequest,
  WorkbenchRequestError,
} from '@/lib/workbench-client';
import { paymentWalletNetwork, useStandardWallet } from '@/lib/wallet-standard';
import { createPaymentFetch } from '@/lib/x402';
import {
  ServiceContractNote,
  WorkbenchEmpty,
  WorkbenchError,
  WorkbenchLoading,
  WorkbenchPageHeader,
  WorkbenchStatus,
} from './workbench-primitives';
import { OrganizationRepositorySelector } from './organization-repository-selector';

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
  const paymentAccount = useWorkbenchResource('/v1/account', paymentAccountId);
  const [selected, setSelected] = useState<string>(
    initialOwner && initialRepo ? `${initialOwner}/${initialRepo}` : '',
  );
  const [repositoryLocked, setRepositoryLocked] = useState(false);
  const repository =
    repositories.status === 'ready'
      ? findSelectedRepository(repositories.data, selected)
      : undefined;
  const repositoryOutage =
    repositories.status === 'ready' &&
    repositories.data.some((item) => item.readiness === 'unavailable');

  useEffect(() => {
    if (selected || repositories.status !== 'ready' || paymentAccount.status !== 'ready') return;
    const recovery = loadWorkbenchPaymentRecovery(paymentAccount.data);
    if (!recovery) return;
    const recoverable = repositories.data.find(
      (item) =>
        item.readiness === 'ready' &&
        item.fullName.toLowerCase() === recovery.repository.toLowerCase(),
    );
    if (recoverable) setSelected(recoverable.fullName);
  }, [paymentAccount, repositories, selected]);

  useEffect(() => {
    if (repository && selected !== repository.fullName) setSelected(repository.fullName);
  }, [repository, selected]);

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
                  <span>Organization and repository</span>
                  <h2>Choose an organization and ready repository</h2>
                </div>
                {repository && <WorkbenchStatus value={repository.readiness} />}
              </div>
              <OrganizationRepositorySelector
                repositories={repositories.data}
                selected={selected}
                disabled={repositoryLocked}
                onSelect={setSelected}
              />
              {selected && !repository && (
                <p className="wizard-help">
                  That repository is not connected to this account.{' '}
                  <Link href="/app/onboarding">Review connected repositories</Link>
                </p>
              )}
              {repository && repository.readiness !== 'ready' && (
                <p className="wizard-help">
                  {repository.readiness === 'unavailable'
                    ? 'Repository readiness could not be confirmed. '
                    : 'Finish the repository setup before requesting a quote. '}
                  {repository.readiness === 'unavailable' ? (
                    <button type="button" onClick={repositories.refresh}>
                      Retry status
                    </button>
                  ) : (
                    <Link
                      href={`/app/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}`}
                    >
                      Review requirements
                    </Link>
                  )}
                </p>
              )}
              {repositoryOutage && !selected && (
                <p className="wizard-help">
                  One or more repository checks are temporarily unavailable.{' '}
                  <button type="button" onClick={repositories.refresh}>
                    Retry status
                  </button>
                </p>
              )}
              {repositoryLocked && (
                <p className="wizard-help">
                  Check the current payment status before changing repositories.
                </p>
              )}
            </div>
          </section>

          {repository?.readiness === 'ready' && (
            <IssueAndPayment
              repository={repository}
              initialIssue={initialIssue}
              accountId={paymentAccount.status === 'ready' ? paymentAccount.data : undefined}
              accountLoading={paymentAccount.status === 'loading'}
              refreshAccount={paymentAccount.refresh}
              onPaymentLockChange={setRepositoryLocked}
            />
          )}
        </>
      ) : null}
      <ServiceContractNote />
    </div>
  );
}

export function findSelectedRepository(
  repositories: readonly WorkbenchRepository[],
  selected: string,
): WorkbenchRepository | undefined {
  const target = selected.trim().toLowerCase();
  if (!target) return undefined;
  return repositories.find((repository) => repository.fullName.toLowerCase() === target);
}

function IssueAndPayment({
  repository,
  initialIssue,
  accountId,
  accountLoading,
  refreshAccount,
  onPaymentLockChange,
}: {
  repository: WorkbenchRepository;
  initialIssue?: number;
  accountId?: string;
  accountLoading: boolean;
  refreshAccount: () => void;
  onPaymentLockChange: (locked: boolean) => void;
}) {
  const router = useRouter();
  const issues = useWorkbenchResource(
    `/v1/repositories/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.repo)}/issues`,
    normalizeIssues,
  );
  const [issueUrl, setIssueUrl] = useState('');
  const [preflight, setPreflight] = useState<WorkbenchPreflight | null>(null);
  const [quote, setQuote] = useState<Quote | null>(null);
  const [state, setState] = useState<
    | 'idle'
    | 'checking'
    | 'quoting'
    | 'quoted'
    | 'paying'
    | 'payment_uncertain'
    | 'checking_payment'
    | 'payment_unpaid'
    | 'revalidating_payment'
  >('idle');
  const [error, setError] = useState<string | null>(null);
  const paymentStatusController = useRef<AbortController | null>(null);
  const {
    wallets,
    connected,
    ready: walletReady,
    connecting,
    error: walletError,
    connect,
    disconnect,
  } = useStandardWallet('transaction');

  useEffect(() => {
    if (issues.status !== 'ready' || issueUrl) return;
    const initial = issues.data.find((item) => item.number === initialIssue);
    if (initial) setIssueUrl(initial.url);
  }, [initialIssue, issueUrl, issues]);

  useEffect(() => {
    if (!accountId) {
      setIssueUrl('');
      setPreflight(null);
      setQuote(null);
      setError(null);
      setState('idle');
      return;
    }
    const recovery = loadWorkbenchPaymentRecovery(accountId);
    if (recovery?.repository.toLowerCase() === repository.fullName.toLowerCase()) {
      setIssueUrl(recovery.issueUrl);
      setPreflight(null);
      setQuote(recovery.quote);
      setError(null);
      setState(
        recovery.phase === 'unpaid'
          ? 'payment_unpaid'
          : recovery.phase === 'prepared'
            ? 'quoted'
            : 'payment_uncertain',
      );
      return;
    }
    setIssueUrl('');
    setPreflight(null);
    setQuote(null);
    setError(null);
    setState('idle');
  }, [accountId, repository.fullName]);

  useEffect(() => {
    return () => paymentStatusController.current?.abort();
  }, [accountId, repository.fullName]);

  const paymentLocked =
    state === 'paying' ||
    state === 'payment_uncertain' ||
    state === 'checking_payment' ||
    state === 'revalidating_payment';

  useEffect(() => {
    onPaymentLockChange(paymentLocked);
    return () => onPaymentLockChange(false);
  }, [onPaymentLockChange, paymentLocked]);

  const selectedIssue =
    issues.status === 'ready' ? issues.data.find((item) => item.url === issueUrl) : undefined;
  const issueSelectionLocked =
    state === 'checking' || state === 'quoting' || state === 'paying' || paymentLocked;

  function selectIssue(next: string) {
    if (quote && accountId) clearWorkbenchPaymentRecovery(accountId, quote.id);
    setIssueUrl(next);
    setPreflight(null);
    setQuote(null);
    setError(null);
    setState('idle');
  }

  async function runPreflight(event?: React.FormEvent<HTMLFormElement>) {
    event?.preventDefault();
    if (quote && accountId) clearWorkbenchPaymentRecovery(accountId, quote.id);
    if (!issueMatchesRepository(issueUrl, repository.fullName)) {
      setState('idle');
      setError(`Choose an issue from ${repository.fullName} before checking maintenance scope.`);
      setPreflight(null);
      setQuote(null);
      return;
    }
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
    if (quote && accountId) clearWorkbenchPaymentRecovery(accountId, quote.id);
    if (!issueMatchesRepository(issueUrl, repository.fullName)) {
      setState('idle');
      setError(`Choose an issue from ${repository.fullName} before requesting a quote.`);
      setQuote(null);
      return;
    }
    setState('quoting');
    setError(null);
    setQuote(null);
    try {
      const next = await workbenchMutation<Quote>('/v1/account/quotes', {
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
    if (!quote || !connected || !walletReady) return;
    if (!accountId) {
      setError('Payment is unavailable until the signed-in GitHub account can be verified.');
      return;
    }
    if (
      !quoteMatchesIssue(quote, issueUrl) ||
      !issueMatchesRepository(issueUrl, repository.fullName)
    ) {
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

    let recovery: WorkbenchPaymentRecovery | undefined;
    let walletSigned = false;
    setError(null);
    try {
      recovery = prepareWorkbenchPaymentRecovery({
        accountId,
        repository: `${quote.owner}/${quote.repo}`,
        issueUrl,
        quote,
      });
      const walletFeature = connected.wallet.features[
        'solana:signTransaction'
      ] as SolanaSignTransactionFeature['solana:signTransaction'];
      const feature: SolanaSignTransactionFeature['solana:signTransaction'] = {
        ...walletFeature,
        async signTransaction(...inputs) {
          const signed = await walletFeature.signTransaction(...inputs);
          walletSigned = true;
          return signed;
        },
      };
      const paidFetch = createPaymentFetch({
        account: connected.account,
        feature,
        quotePayment: quote.payment,
        quoteAmount: quote.priceAtomic,
      });
      recovery = { ...recovery, phase: 'attempting' };
      saveWorkbenchPaymentRecovery(recovery);
      setState('paying');
      const response = await paidFetch('/api/mizuki/v1/jobs', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': recovery.idempotencyKey,
        },
        body: JSON.stringify({ quote_id: quote.id }),
      });
      const job = await readJsonResponse<Job>(response);
      clearWorkbenchPaymentRecovery(accountId, quote.id);
      router.push(`/app/jobs/${encodeURIComponent(job.id)}`);
    } catch (cause) {
      if (!walletSigned) {
        clearWorkbenchPaymentRecovery(accountId, quote.id);
        setState('quoted');
        setError(paymentAttemptError(cause));
        return;
      }
      if (recovery) {
        try {
          saveWorkbenchPaymentRecovery({ ...recovery, phase: 'uncertain' });
        } catch {
          // The read-verified attempting record remains the recovery source.
        }
      }
      setState('payment_uncertain');
      setError(null);
    }
  }

  async function checkPaymentStatus() {
    if (!quote || !accountId) return;
    const recovery = loadWorkbenchPaymentRecovery(accountId);
    if (!recovery || recovery.quote.id !== quote.id) {
      setState('payment_uncertain');
      setError(
        'The secure payment recovery record is unavailable. Do not approve another payment in this tab.',
      );
      return;
    }
    paymentStatusController.current?.abort();
    const controller = new AbortController();
    paymentStatusController.current = controller;
    setState('checking_payment');
    setError(null);
    try {
      const status = await checkQuotePaymentStatus(quote.id, recovery.idempotencyKey, {
        signal: controller.signal,
      });
      if (status.status === 'job_reserved') {
        clearWorkbenchPaymentRecovery(accountId, quote.id);
        router.push(`/app/jobs/${encodeURIComponent(status.job.id)}`);
        return;
      }
      if (status.expiresAt !== quote.expiresAt) {
        throw new Error('Payment status did not match the accepted quote');
      }
      saveWorkbenchPaymentRecovery({ ...recovery, phase: 'unpaid' });
      setState('payment_unpaid');
    } catch (cause) {
      if (controller.signal.aborted) return;
      try {
        saveWorkbenchPaymentRecovery({ ...recovery, phase: 'uncertain' });
      } catch {
        // The earlier recovery record retains the exact idempotency key.
      }
      setState('payment_uncertain');
      setError(
        cause instanceof Error
          ? `${cause.message} No new payment was requested.`
          : 'Payment status is unavailable. No new payment was requested.',
      );
    } finally {
      if (paymentStatusController.current === controller) {
        paymentStatusController.current = null;
      }
    }
  }

  async function revalidatePaymentRetry() {
    if (!quote || !accountId) return;
    setState('revalidating_payment');
    setError(null);
    try {
      const value = await workbenchRequest<unknown>('/v1/preflights', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: issueUrl.trim() }),
      });
      const next = normalizePreflight(value);
      clearWorkbenchPaymentRecovery(accountId, quote.id);
      setPreflight(next);
      setQuote(null);
      setState('idle');
    } catch (cause) {
      setState('payment_unpaid');
      setError(preflightError(cause));
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
              <button type="submit" disabled={issueSelectionLocked || !issueUrl}>
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
                      : preflight.eligibility === 'unavailable'
                        ? 'Readiness temporarily unavailable'
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
            {preflight.eligibility === 'unavailable' ? (
              <button type="button" onClick={() => void runPreflight()}>
                Retry readiness
              </button>
            ) : preflight.eligibility !== 'ready' ? (
              <a href={preflight.issue.url} target="_blank" rel="noreferrer">
                Update issue on GitHub ↗
              </a>
            ) : null}
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
                  <h2>
                    {state === 'payment_uncertain' || state === 'checking_payment'
                      ? 'Confirm the existing payment status'
                      : state === 'payment_unpaid' || state === 'revalidating_payment'
                        ? 'Recheck eligibility before retrying'
                        : 'Pay the exact quote and start'}
                  </h2>
                </div>
              </div>
              {!accountId ? (
                <div className="wizard-payment-recovery" role="status">
                  <strong>
                    {accountLoading
                      ? 'Verifying the signed-in GitHub account…'
                      : 'Payment is temporarily unavailable'}
                  </strong>
                  <p>
                    The signed-in account must be verified before this tab can create a secure
                    payment recovery record or open a wallet approval.
                  </p>
                  {!accountLoading && (
                    <button type="button" onClick={refreshAccount}>
                      Retry account verification
                    </button>
                  )}
                </div>
              ) : state === 'payment_uncertain' || state === 'checking_payment' ? (
                <PaymentRecoveryNotice
                  checking={state === 'checking_payment'}
                  quoteId={quote.id}
                  check={() => void checkPaymentStatus()}
                />
              ) : state === 'payment_unpaid' || state === 'revalidating_payment' ? (
                <div className="wizard-payment-recovery confirmed" role="status">
                  <strong>No payment or job was found</strong>
                  <p>
                    {quoteExpired(quote)
                      ? 'This quote has expired.'
                      : 'The previous attempt did not create a payment or reserve a job.'}{' '}
                    Recheck issue eligibility and request a new fixed quote before opening the
                    wallet again.
                  </p>
                  <button
                    type="button"
                    disabled={state === 'revalidating_payment'}
                    onClick={() => void revalidatePaymentRetry()}
                  >
                    {state === 'revalidating_payment'
                      ? 'Rechecking eligibility…'
                      : 'Recheck issue eligibility'}
                  </button>
                </div>
              ) : (
                <>
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
                            <strong>
                              {connecting === wallet.name ? 'Connecting…' : 'Connect ↗'}
                            </strong>
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
                    <ConnectedPaymentSummary
                      address={connected.account.address}
                      amountAtomic={quote.priceAtomic}
                      ready={walletReady}
                      paying={state === 'paying'}
                      changeWallet={() => void disconnect()}
                      pay={() => void payAndStart()}
                    />
                  )}
                  <p className="wizard-consent">
                    By paying, you accept the <Link href="/terms">service terms</Link> and
                    acknowledge the <Link href="/privacy">privacy notice</Link>.
                  </p>
                </>
              )}
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

export function ConnectedPaymentSummary({
  address,
  amountAtomic,
  ready,
  paying,
  changeWallet,
  pay,
}: {
  address: string;
  amountAtomic: string;
  ready: boolean;
  paying: boolean;
  changeWallet: () => void;
  pay: () => void;
}) {
  return (
    <div className="wizard-connected-payment">
      <div className="wizard-connected-payment-heading">
        <div>
          <span>Connected payer</span>
          <strong title={address}>{truncateAddress(address, 7)}</strong>
        </div>
        <button type="button" disabled={paying} onClick={changeWallet}>
          Change wallet
        </button>
      </div>
      <dl>
        <div>
          <dt>Network</dt>
          <dd>{paymentWalletNetwork().label}</dd>
        </div>
        <div>
          <dt>Asset</dt>
          <dd>USDC</dd>
        </div>
        <div>
          <dt>Exact amount</dt>
          <dd>{formatUsdcAtomic(amountAtomic)}</dd>
        </div>
      </dl>
      <p>
        {ready
          ? 'Wallet account and disconnect changes are being monitored.'
          : 'Waiting for the wallet account subscription before payment can begin.'}
      </p>
      <button className="wizard-pay-button" type="button" disabled={paying || !ready} onClick={pay}>
        {paying ? 'Confirming payment…' : `Pay ${formatUsdcAtomic(amountAtomic)} and start`}
      </button>
    </div>
  );
}

export function PaymentRecoveryNotice({
  checking,
  quoteId,
  check,
}: {
  checking: boolean;
  quoteId: string;
  check: () => void;
}) {
  return (
    <div className="wizard-payment-recovery" role="status" aria-live="polite">
      <strong>
        {checking ? 'Checking the existing record…' : 'Payment confirmation was interrupted'}
      </strong>
      <p>
        Do not approve another payment yet. This check only reads the existing quote and job record;
        it never opens the wallet or requests a signature.
      </p>
      <code>Quote {quoteId}</code>
      <button type="button" disabled={checking} onClick={check}>
        {checking ? 'Checking payment status…' : 'Check payment status'}
      </button>
    </div>
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
      aria-pressed={selected}
      onClick={select}
      disabled={disabled}
    >
      <span>Issue #{issue.number}</span>
      <strong>{issue.title}</strong>
      <small>
        {issue.reason ||
          (issue.eligibility === 'unavailable'
            ? 'Authorization status is temporarily unavailable'
            : issue.authorized
              ? 'Maintainer authorization confirmed'
              : 'Authorization label required')}
      </small>
    </button>
  );
}

function WizardProgress({ repository }: { repository?: WorkbenchRepository }) {
  return (
    <ol className="wizard-progress" aria-label="New job progress">
      {['Organization & repo', 'Issue', 'Preflight', 'Contract', 'Pay'].map((label, index) => (
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

function paymentAttemptError(cause: unknown): string {
  if (cause instanceof PaymentRecoveryStorageError) return cause.message;
  if (cause instanceof Error && cause.message) {
    return `${cause.message} No payment or job was created.`;
  }
  return 'Payment could not start. No payment or job was created.';
}
