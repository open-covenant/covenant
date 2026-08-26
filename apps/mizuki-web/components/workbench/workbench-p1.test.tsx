import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { BillingEntryRow } from './billing';
import { MachineTokenRecord } from './account-surfaces';
import { ConnectedPaymentSummary } from './new-job-wizard';
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

  it('shows created and exact revoked token history without a revoke control', () => {
    const html = renderToStaticMarkup(
      <MachineTokenRecord
        token={{
          id: '11111111-1111-4111-8111-111111111111',
          name: 'Release MCP',
          prefix: 'mzk_v1_abcdefghijkl',
          scopes: ['account:jobs:read'],
          state: 'revoked',
          createdAt: '2026-08-25T10:00:00.000Z',
          expiresAt: '2026-11-25T10:00:00.000Z',
          lastUsedAt: '2026-08-25T10:05:00.000Z',
          revokedAt: '2026-08-25T10:30:00.000Z',
        }}
        pending={false}
        revoke={vi.fn()}
      />,
    );

    expect(html).toContain(
      '<dt>Created</dt><dd><time dateTime="2026-08-25T10:00:00.000Z">Aug 25, 2026, 10:00 UTC</time></dd>',
    );
    expect(html).toContain(
      '<dt>Revoked</dt><dd><time dateTime="2026-08-25T10:30:00.000Z">Aug 25, 2026, 10:30 UTC</time></dd>',
    );
    expect(html).toContain('Revoked');
    expect(html).not.toContain('>Revoke</button>');
  });
});
