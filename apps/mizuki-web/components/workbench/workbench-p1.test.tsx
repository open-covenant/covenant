import { readFileSync } from 'node:fs';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { BillingEntryRow } from './billing';
import {
  ConnectedPaymentSummary,
  paymentAttemptError,
  PaymentRecoveryNotice,
  paymentStatusError,
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
        check={vi.fn()}
      />,
    );

    expect(html).toContain('Checking your payment status');
    expect(html).toContain('never opens the wallet or requests a signature');
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
    expect(paymentAttempt).toContain('await assertPaymentBalance');
    expect(paymentAttempt).toContain('payment_attempt_id: attempt.id');
    expect(paymentAttempt).toContain('await resolvePaymentRecovery(');
    expect(paymentAttempt).toContain(
      '!current.retrySafe && !paymentPromptRetryAllowed(recovery, current)',
    );
    expect(paymentAttempt).toContain('clearWorkbenchPaymentRecovery(accountId, quote.id)');
    expect(paymentAttempt.indexOf('await assertPaymentBalance')).toBeLessThan(
      paymentAttempt.indexOf('await createPaymentAttempt'),
    );
    expect(paymentAttempt.indexOf('await createPaymentAttempt')).toBeLessThan(
      paymentAttempt.indexOf('createPaymentFetch'),
    );
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
