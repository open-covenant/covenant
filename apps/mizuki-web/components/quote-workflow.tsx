'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';
import type { SolanaSignTransactionFeature } from '@solana/wallet-standard-features';
import { formatUsdcAtomic } from '@/lib/format';
import { githubIssuePattern } from '@/lib/github-url';
import type { Job, Quote } from '@/lib/types';
import { useStandardWallet } from '@/lib/wallet-standard';
import { createPaymentFetch } from '@/lib/x402';

type RequestState = 'idle' | 'quoting' | 'quoted' | 'paying';

async function readResponse<T>(response: Response): Promise<T> {
  const body = (await response.json().catch(() => ({}))) as T & { error?: string; reason?: string };
  if (!response.ok)
    throw new Error(body.error || body.reason || `Request failed (${response.status})`);
  return body;
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
      const next = await readResponse<Quote>(response);
      setQuote(next);
      setState('quoted');
    } catch (cause) {
      setState('idle');
      setError(cause instanceof Error ? cause.message : 'Mizuki could not quote this issue');
    }
  }

  async function payAndRun() {
    if (!quote || !connected) return;
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
      const job = await readResponse<Job>(response);
      router.push(`/jobs/${encodeURIComponent(job.id)}`);
    } catch (cause) {
      setState('quoted');
      setError(cause instanceof Error ? cause.message : 'Payment could not be completed');
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
            disabled={state === 'quoting' || state === 'paying'}
          >
            {state === 'quoting' ? 'Inspecting…' : quote ? 'Refresh quote' : 'Get fixed quote'}
          </button>
        </div>
        <p id="issue-help">
          Mizuki reads issue scope and repository metadata. Requesting a quote never creates a pull
          request.
        </p>
      </form>

      {quote && (
        <section className="quote-result" aria-live="polite">
          <div className="quote-result-heading">
            <div>
              <span>Fixed quote</span>
              <strong>{formatUsdcAtomic(quote.priceAtomic)}</strong>
            </div>
            <span className="quote-class">{quote.class}</span>
          </div>
          <div className="quote-issue">
            <p>
              {quote.owner}/{quote.repo} · issue #{quote.issueNumber}
            </p>
            <h2>{quote.issueTitle}</h2>
          </div>
          <dl className="quote-limits">
            <div>
              <dt>Maximum files</dt>
              <dd>{quote.maxFiles}</dd>
            </div>
            <div>
              <dt>Variable execution estimate ceiling</dt>
              <dd>${quote.maxCostUsd.toFixed(2)}</dd>
            </div>
            <div>
              <dt>Guarantee</dt>
              <dd>PR or full refund</dd>
            </div>
          </dl>
          <div className="payment-step">
            <div>
              <p className="eyebrow">Pay with a Solana wallet</p>
              <p>
                Your wallet signs a USDC payment capped at the exact quote. The job starts after
                settlement.
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
                      <span>{connecting === wallet.name ? 'Connecting…' : 'Connect ↗'}</span>
                    </button>
                  ))}
                </div>
              ) : (
                <p className="wallet-missing">
                  No Wallet Standard-compatible Solana wallet was detected.
                </p>
              )
            ) : (
              <button
                className="button button-primary pay-button"
                type="button"
                disabled={state === 'paying'}
                onClick={() => void payAndRun()}
              >
                {state === 'paying'
                  ? 'Settling payment…'
                  : `Pay ${formatUsdcAtomic(quote.priceAtomic)} and start`}
              </button>
            )}
          </div>
        </section>
      )}

      {(error || walletError) && (
        <p className="form-error" role="alert">
          {error || walletError}
        </p>
      )}
    </div>
  );
}
