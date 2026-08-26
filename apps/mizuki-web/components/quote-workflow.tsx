'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';
import type { SolanaSignTransactionFeature } from '@solana/wallet-standard-features';
import { formatTime, formatUsdcAtomic, stateLabel } from '@/lib/format';
import { githubIssuePattern } from '@/lib/github-url';
import type { Job, Quote } from '@/lib/types';
import { useStandardWallet } from '@/lib/wallet-standard';
import { createPaymentFetch, paymentPreparationError } from '@/lib/x402';

type RequestState = 'idle' | 'quoting' | 'quoted' | 'paying' | 'payment_uncertain';

class CustomerRequestError extends Error {}

async function readResponse<T>(
  response: Response,
  action: 'quote' | 'payment',
  quoteId?: string,
): Promise<T> {
  const body = (await response.json().catch(() => ({}))) as T & { error?: unknown };
  if (!response.ok) {
    throw new CustomerRequestError(
      requestError(
        response.status,
        action,
        quoteId,
        typeof body.error === 'string' ? body.error : undefined,
      ),
    );
  }
  return body;
}

function requestError(
  status: number,
  action: 'quote' | 'payment',
  quoteId?: string,
  detail = '',
): string {
  if (action === 'quote') {
    if (status === 409)
      return 'The issue or repository changed. Request a new quote and try again.';
    if (status === 422) {
      if (/install|installation|policy verifier/i.test(detail)) {
        return 'Install both required GitHub Apps on this repository, then request a new quote.';
      }
      if (/authoriz|label/i.test(detail)) {
        return 'This issue has not been authorized for paid work. Open its repository in Workbench and choose Authorize, then request a new quote.';
      }
      if (/public github|private repository/i.test(detail)) {
        return 'Mizuki accepts public GitHub repositories only.';
      }
      if (/too large|max files|file limit/i.test(detail)) {
        return 'This issue is too large for the fixed-price service. Reduce it to one small maintenance task.';
      }
      if (/outside.*scope|feature|enhancement|sensitive/i.test(detail)) {
        return 'This issue falls outside the supported maintenance scope. Submit a focused bug fix, test, documentation, lint, type, or configuration repair.';
      }
      return 'This issue is not eligible. Confirm the required Apps, authorize the issue in Workbench, and check that it is public and within the supported scope.';
    }
    if (status === 429) return 'Too many quote requests. Wait a moment and try again.';
    return 'We could not create a quote. No payment was requested.';
  }
  return `Payment status could not be confirmed. Do not submit another payment. Check your wallet activity, then contact support with quote ${quoteId ?? 'ID shown above'}.`;
}

function paymentKey(quoteId: string): string {
  const storageKey = `mizuki:payment:${quoteId}`;
  const existing = window.sessionStorage.getItem(storageKey);
  if (existing) return existing;
  const key = crypto.randomUUID();
  window.sessionStorage.setItem(storageKey, key);
  return key;
}

export function QuoteWorkflow() {
  const router = useRouter();
  const [issueUrl, setIssueUrl] = useState('');
  const [quote, setQuote] = useState<Quote | null>(null);
  const [state, setState] = useState<RequestState>('idle');
  const [error, setError] = useState<string | null>(null);
  const {
    wallets,
    connected,
    connecting,
    error: walletError,
    connect,
  } = useStandardWallet('transaction');

  async function requestQuote(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError(null);
    setState('quoting');
    try {
      const response = await fetch('/api/mizuki/v1/quotes', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ github_issue_url: issueUrl.trim() }),
      });
      const next = await readResponse<Quote>(response, 'quote');
      setQuote(next);
      setState('quoted');
    } catch (cause) {
      setState('idle');
      setError(cause instanceof CustomerRequestError ? cause.message : requestError(0, 'quote'));
    }
  }

  async function payAndRun() {
    if (!quote || !connected) return;
    setState('paying');
    setError(null);
    let walletSigned = false;
    try {
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
      const response = await paidFetch('/api/mizuki/v1/jobs', {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'idempotency-key': paymentKey(quote.id),
        },
        body: JSON.stringify({ quote_id: quote.id }),
      });
      const job = await readResponse<Job>(response, 'payment', quote.id);
      router.push(`/jobs/${encodeURIComponent(job.id)}`);
    } catch (cause) {
      const failure = publicPaymentFailure(cause, walletSigned, quote.id, quote.priceAtomic);
      setState(failure.uncertain ? 'payment_uncertain' : 'quoted');
      setError(failure.message);
    }
  }

  return (
    <div className="quote-workflow">
      <form className="issue-form" onSubmit={requestQuote}>
        <label htmlFor="github-issue">Public GitHub issue URL</label>
        <div className="issue-input-row">
          <input
            id="github-issue"
            type="url"
            inputMode="url"
            placeholder="https://github.com/owner/repository/issues/123"
            value={issueUrl}
            onChange={(event) => setIssueUrl(event.target.value)}
            required
            pattern={githubIssuePattern}
            aria-describedby="issue-help"
          />
          <button
            className="button button-primary"
            type="submit"
            disabled={state === 'quoting' || state === 'paying' || state === 'payment_uncertain'}
          >
            {state === 'quoting'
              ? 'Checking issue…'
              : quote
                ? 'Request updated quote'
                : 'Get fixed quote'}
          </button>
        </div>
        <p id="issue-help">
          Before continuing, connect the repository and authorize the issue in{' '}
          <a href="/app">Mizuki Workbench</a>. A quote reads public issue and repository metadata
          but does not create a branch or pull request.
        </p>
      </form>

      {quote && (
        <section className="quote-result" aria-live="polite">
          <div className="quote-result-heading">
            <div>
              <span>Fixed quote</span>
              <strong>{formatUsdcAtomic(quote.priceAtomic)}</strong>
            </div>
            <span className="quote-class">{stateLabel(quote.class)}</span>
          </div>
          <div className="quote-issue">
            <p>
              {quote.owner}/{quote.repo} · issue #{quote.issueNumber}
            </p>
            <h2>{quote.issueTitle}</h2>
          </div>
          <dl className="quote-limits">
            <div>
              <dt>Maximum files changed</dt>
              <dd>{quote.maxFiles}</dd>
            </div>
            <div>
              <dt>Quote expires</dt>
              <dd>{formatTime(quote.expiresAt)}</dd>
            </div>
            <div>
              <dt>Guarantee</dt>
              <dd>Validated pull request or full refund of the quoted USDC amount</dd>
            </div>
          </dl>
          <div className="payment-step">
            <div>
              <p className="eyebrow">Pay with a Solana wallet</p>
              <p>
                Your wallet authorizes a {formatUsdcAtomic(quote.priceAtomic)} transfer on Solana.
                The recipient and amount are bound to this quote, and work starts only after payment
                is confirmed. Your wallet may charge separate SOL network or service fees; those
                fees are not included in a refund.
              </p>
              <p className="payment-consent">
                By paying, you agree to the <a href="/terms">Service terms</a> and acknowledge the{' '}
                <a href="/privacy">Privacy and data use notice</a>.
              </p>
            </div>
            {!connected ? (
              wallets.length ? (
                <div className="wallet-options compact-wallet-options">
                  {wallets.map((wallet) => (
                    <button
                      type="button"
                      key={wallet.name}
                      disabled={Boolean(connecting)}
                      onClick={() => void connect(wallet)}
                    >
                      <span>{wallet.name}</span>
                      <span>{connecting === wallet.name ? 'Connecting…' : 'Connect'}</span>
                    </button>
                  ))}
                </div>
              ) : (
                <p className="wallet-missing">
                  No compatible Solana wallet was detected. Install or enable a wallet that supports
                  Solana transactions, then reload this page.
                </p>
              )
            ) : (
              <button
                className="button button-primary pay-button"
                type="button"
                disabled={state === 'paying' || state === 'payment_uncertain'}
                onClick={() => void payAndRun()}
              >
                {state === 'paying'
                  ? 'Settling payment…'
                  : state === 'payment_uncertain'
                    ? 'Check payment status before retrying'
                    : `Pay ${formatUsdcAtomic(quote.priceAtomic)} and start`}
              </button>
            )}
          </div>
        </section>
      )}

      {(error || walletError) && (
        <p className="form-error" role="alert">
          {error ||
            (walletError
              ? 'We could not connect to that wallet. Check the wallet and try again.'
              : '')}
        </p>
      )}
    </div>
  );
}

export function publicPaymentFailure(
  cause: unknown,
  walletSigned: boolean,
  quoteId: string,
  amountAtomic: string,
): { message: string; uncertain: boolean } {
  const uncertain = walletSigned || cause instanceof CustomerRequestError;
  return {
    uncertain,
    message: uncertain
      ? cause instanceof CustomerRequestError
        ? cause.message
        : requestError(0, 'payment', quoteId)
      : paymentPreparationError(cause, formatUsdcAtomic(amountAtomic)),
  };
}
