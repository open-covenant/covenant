import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ useWorkbenchResource: vi.fn() }));

vi.mock('@/lib/workbench-client', () => ({
  useWorkbenchResource: mocks.useWorkbenchResource,
}));

import { Billing } from './billing';

describe('Workbench billing', () => {
  it('separates confirming payments from finalized paid totals', () => {
    mocks.useWorkbenchResource.mockReturnValue({
      status: 'ready',
      refresh: vi.fn(),
      data: {
        limit: 1_000,
        truncated: true,
        obligationCount: 1,
        totalsScope: 'latest_terminal_jobs_and_all_obligations',
        entries: [
          {
            id: 'payment:confirming',
            kind: 'payment',
            state: 'pending',
            amountAtomic: '3000000',
            asset: 'USDC',
            jobId: 'confirming',
            repository: 'example/project',
            occurredAt: '2026-08-25T12:00:00.000Z',
          },
          {
            id: 'payment:finalized',
            kind: 'payment',
            state: 'finalized',
            amountAtomic: '2000000',
            asset: 'USDC',
            jobId: 'finalized',
            repository: 'example/project',
            transaction: 'settlement',
            occurredAt: '2026-08-25T11:00:00.000Z',
          },
        ],
      },
    });

    const html = renderToStaticMarkup(<Billing />);

    expect(html).toContain('<span>Payments confirming</span><strong>1</strong>');
    expect(html).toContain('<span>Paid</span><strong>2 USDC</strong>');
    expect(html).not.toContain('<span>Paid</span><strong>5 USDC</strong>');
    expect(html).toContain('Awaiting settlement confirmation; do not pay again');
    expect(html).toContain('Every payment or refund still in progress is included');
  });
});
