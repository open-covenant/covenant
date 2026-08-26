import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { BillingEntryRow } from './billing';
import { ConnectedPaymentSummary, PaymentRecoveryNotice } from './new-job-wizard';
import { WorkbenchNavLink } from './workbench-shell';

describe('Workbench responsive records and controls', () => {
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
});
