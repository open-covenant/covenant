import { readFileSync } from 'node:fs';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { BillingEntryRow } from './billing';
import {
  ConnectedPaymentSummary,
  paymentAttemptBlocksQuote,
  paymentAttemptError,
  PaymentRecoveryNotice,
  paymentStatusError,
  recoveryFromAttempt,
} from './new-job-wizard';
import { WorkbenchRequestError } from '../../lib/workbench-client';
import { WorkbenchHeader, WorkbenchNavLink } from './workbench-shell';

describe('Workbench responsive records and controls', () => {
  it('keeps the shared payment wallet control in the Workbench header', () => {
    const html = renderToStaticMarkup(
      <WorkbenchHeader walletControl={<button type="button">Connect wallet</button>} />,
    );

    expect(html).toContain('workbench-header');
    expect(html).toContain('Maintenance workbench');
    expect(html).toContain('href="/app/jobs/new"');
    expect(html).toContain('Connect wallet');
  });

  it('keeps navigation icons distinguishable while preserving visible labels', () => {
    const html = renderToStaticMarkup(
      <WorkbenchNavLink
        item={{ href: '/app/billing', label: 'Payments & refunds', icon: '$' }}
        pathname="/app/billing"
      />,
    );

    expect(html).toContain('aria-current="page"');
    expect(html).toContain('aria-hidden="true"');
    expect(html).toContain('Payments &amp; refunds');
    expect(html).toContain('workbench-nav-icon');
  });

  it('does not advertise a second quote flow outside Workbench', () => {
    const source = readFileSync(new URL('./workbench-shell.tsx', import.meta.url), 'utf8');

    expect(source).toContain('Learn how paid maintenance works');
    expect(source).not.toContain('Request a quote without opening Workbench');
  });

  it('keeps recorded time and transaction evidence in every billing row', () => {
    const html = renderToStaticMarkup(
      <BillingEntryRow
        entry={{
          id: 'payment-1',
          kind: 'payment',
          state: 'finalized',
          amountAtomic: '2000000',
          asset: 'USDC',
          jobId: '11111111-1111-4111-8111-111111111111',
          repository: 'open-covenant/covenant',
          transaction: 'settlement-signature',
          occurredAt: '2026-08-25T00:00:00.000Z',
        }}
      />,
    );

    expect(html).toContain('Recorded');
    expect(html).toContain('Evidence');
    expect(html).toContain('Transaction ↗');
    expect(html).toContain('https://solscan.io/tx/settlement-signature');
  });

  it('shows the exact payment context and waits for account observation', () => {
    const html = renderToStaticMarkup(
      <ConnectedPaymentSummary
        address="2n1fD5H61zB8Qsg9iswBteVPWzr3pzWEeTXejXtuTn2E"
        amountAtomic="2000000"
        ready={false}
        paying={false}
        changeWallet={vi.fn()}
        pay={vi.fn()}
      />,
    );

    expect(html).toContain('2n1fD5H…XtuTn2E');
    expect(html).toContain('Solana mainnet');
    expect(html).toContain('USDC');
    expect(html).toContain('2 USDC');
    expect(html).toContain('Change wallet');
    expect(html).toContain('before payment can begin');
    expect(html).toContain('disabled=""');
  });

  it('explains the read-only payment recovery check without prompting another payment', () => {
    const html = renderToStaticMarkup(
      <PaymentRecoveryNotice
        quoteId="c33d57fa-fe99-4afa-a624-543991dcc7cf"
        checking
        progress={{
          startedAtMs: 1_000,
          foregroundDeadlineAtMs: 31_000,
          observedAtMs: 11_000,
          elapsedMs: 10_000,
          remainingMs: 20_000,
          pollCount: 2,
          latestStatus: 'submitting',
          outcome: 'checking',
        }}
        check={vi.fn()}
      />,
    );

    expect(html).toContain('Checking your payment status');
    expect(html).toContain('never opens the wallet or requests a signature');
    expect(html).toContain('role="progressbar"');
    expect(html).toContain('10s / 30s');
    expect(html).toContain('Checking the submitted payment');
    expect(html).toContain('aria-valuenow="33"');
    expect(html).toContain('c33d57fa-fe99-4afa-a624-543991dcc7cf');
    expect(html).not.toContain('Payment confirmation was interrupted');
  });

  it('binds every signed retry to a server attempt and reconciles failures', () => {
    const source = readFileSync(new URL('./new-job-wizard.tsx', import.meta.url), 'utf8');
    const start = source.indexOf('async function payAndStart()');
    const end = source.indexOf('async function checkPaymentStatus()', start);
    const paymentAttempt = source.slice(start, end);

    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    expect(paymentAttempt).toContain('await createPaymentAttempt');
    expect(paymentAttempt).toContain('await findActivePaymentAttempt');
    expect(paymentAttempt).toContain('recoveryFromAttempt(accountId');
    expect(paymentAttempt).toContain('setQuote(paymentQuote)');
    expect(paymentAttempt).toContain('quotePayment: paymentQuote.payment');
    expect(paymentAttempt).toContain('await assertPaymentBalance');
    expect(paymentAttempt).toContain('payment_attempt_id: attempt.id');
    expect(paymentAttempt).toContain('await resolvePaymentRecovery(');
    expect(paymentAttempt).toContain(
      '!current.retrySafe && !paymentPromptRetryAllowed(recovery, current)',
    );
    expect(paymentAttempt).toContain('clearWorkbenchPaymentRecovery(accountId, paymentQuote.id)');
    expect(paymentAttempt.indexOf('await findActivePaymentAttempt')).toBeLessThan(
      paymentAttempt.indexOf('await assertPaymentBalance'),
    );
    expect(paymentAttempt.indexOf('await assertPaymentBalance')).toBeLessThan(
      paymentAttempt.indexOf('await createPaymentAttempt'),
    );
    expect(paymentAttempt.indexOf('await createPaymentAttempt')).toBeLessThan(
      paymentAttempt.indexOf('createPaymentFetch'),
    );
    expect(source.match(/setQuote\(recovered\.quote\)/g)).toHaveLength(2);
  });

  it('adopts a server-owned attempt without reopening an uncertain wallet prompt', () => {
    const quote = {
      id: '11111111-1111-4111-8111-111111111111',
      issueUrl: 'https://github.com/open-covenant/covenant/issues/146',
      owner: 'open-covenant',
      repo: 'covenant',
      issueNumber: 146,
      issueTitle: 'Independent audit request',
      class: 'micro' as const,
      priceAtomic: '2000000',
      maxFiles: 3,
      maxCostUsd: 0.8,
      expiresAt: '2099-01-01T00:00:00.000Z',
      payment: { x402Version: 2 },
    };
    const attempt = {
      id: 'attempt-11111111',
      quoteId: quote.id,
      wallet: 'FTT2gzXLipTfg3ijqiGQRkMHjAA52eYLAoB3TM3e9p8n',
      idempotencyKey: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      stage: 'wallet_opened' as const,
      paymentStatus: 'wallet_opened' as const,
      retrySafe: false,
      promptAuthorization: {
        nonce: '11111111-1111-4111-8111-111111111111',
        authorizedAt: '2026-08-28T21:10:52.000Z',
      },
    };

    expect(recoveryFromAttempt('42', attempt, quote)).toMatchObject({
      phase: 'uncertain',
      walletAuthorized: false,
      wallet: attempt.wallet,
      attemptId: attempt.id,
    });
    expect(
      recoveryFromAttempt(
        '42',
        attempt,
        { ...quote, payment: undefined },
        {
          phase: 'prepared',
          walletAuthorized: false,
          accountId: '42',
          attemptId: attempt.id,
          idempotencyKey: attempt.idempotencyKey,
          promptNonce: attempt.promptAuthorization.nonce,
          repository: `${quote.owner}/${quote.repo}`,
          issueUrl: quote.issueUrl,
          quote,
          wallet: attempt.wallet,
        },
      ),
    ).toMatchObject({ phase: 'prepared', quote: { payment: quote.payment } });
    expect(
      recoveryFromAttempt(
        '42',
        { ...attempt, stage: 'submitting', paymentStatus: 'submitting' },
        quote,
      ),
    ).toMatchObject({ phase: 'uncertain', walletAuthorized: true });
    expect(
      recoveryFromAttempt(
        '42',
        { ...attempt, stage: 'expired_unpaid', paymentStatus: 'expired_unpaid', retrySafe: true },
        quote,
      ),
    ).toMatchObject({ phase: 'unpaid', walletAuthorized: false });
  });

  it('recovers only attempts that can block the selected quote', () => {
    const attempt = {
      quoteId: 'old-quote',
      paymentStatus: 'created' as const,
    };

    expect(paymentAttemptBlocksQuote(attempt, 'current-quote')).toBe(false);
    expect(
      paymentAttemptBlocksQuote({ ...attempt, paymentStatus: 'expired_unpaid' }, 'current-quote'),
    ).toBe(false);
    expect(
      paymentAttemptBlocksQuote({ ...attempt, paymentStatus: 'job_reserved' }, 'current-quote'),
    ).toBe(false);
    expect(
      paymentAttemptBlocksQuote({ ...attempt, paymentStatus: 'wallet_opened' }, 'current-quote'),
    ).toBe(true);
    expect(paymentAttemptBlocksQuote(attempt, 'old-quote')).toBe(true);
  });

  it('replaces a definitively unpaid attempt with a fresh quote in one action', () => {
    const source = readFileSync(new URL('./new-job-wizard.tsx', import.meta.url), 'utf8');
    const start = source.indexOf('async function revalidatePaymentRetry()');
    const end = source.indexOf('\n  return (', start);
    const renewal = source.slice(start, end);

    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    expect(renewal).toContain('clearWorkbenchPaymentRecovery(accountId, previousQuoteId)');
    expect(renewal).toContain('paymentRecovery.current = null');
    expect(renewal).toContain("workbenchRequest<unknown>('/v1/preflights'");
    expect(renewal).toContain("workbenchMutation<Quote>('/v1/account/quotes'");
    expect(renewal).toContain('setQuote(freshQuote)');
    expect(renewal).toContain("setState('quoted')");
    expect(renewal.indexOf("workbenchRequest<unknown>('/v1/preflights'")).toBeLessThan(
      renewal.indexOf("workbenchMutation<Quote>('/v1/account/quotes'"),
    );
    expect(source).toContain('Create a fresh quote');
    expect(source).not.toContain('Recheck issue eligibility');
  });

  it('classifies payment-attempt API failures without blaming the wallet', () => {
    expect(
      paymentAttemptError(
        new WorkbenchRequestError('service dependencies are not ready', 503),
        '2 USDC',
      ),
    ).toBe(
      'The payment service is temporarily unavailable. Try again in a moment. No payment or job was created.',
    );
    expect(paymentAttemptError(new WorkbenchRequestError('quote expired', 409), '2 USDC')).toBe(
      'This quote expired. Refresh the page and request a new fixed quote. No payment or job was created.',
    );
    expect(
      paymentAttemptError(
        new WorkbenchRequestError('resolve the active payment attempt', 409),
        '2 USDC',
      ),
    ).toContain('already has a payment attempt');
    expect(
      paymentAttemptError(
        new WorkbenchRequestError('service dependencies are not ready', 503),
        '2 USDC',
      ),
    ).not.toContain('Reconnect');
    expect(
      paymentAttemptError(
        new WorkbenchRequestError('repository changed; request a new quote', 409),
        '2 USDC',
      ),
    ).toContain('repository or issue changed');
    expect(
      paymentAttemptError(new WorkbenchRequestError('repository access revoked', 403), '2 USDC'),
    ).toContain('repository connection');
    expect(
      paymentAttemptError(new WorkbenchRequestError('payment verification failed', 402), '2 USDC'),
    ).toContain("Check this payment's status");
  });

  it('classifies recovery reads using the error returned by Workbench requests', () => {
    expect(paymentStatusError(new WorkbenchRequestError('quote not found', 404))).toContain(
      'saved quote could not be found',
    );
    expect(paymentStatusError(new WorkbenchRequestError('conflict', 409))).toContain(
      'conflicts with another request',
    );
    expect(paymentStatusError(new WorkbenchRequestError('rate limited', 429))).toContain(
      'being checked too frequently',
    );
  });
});
