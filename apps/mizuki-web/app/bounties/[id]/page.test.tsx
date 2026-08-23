import { renderToStaticMarkup } from 'react-dom/server';
import { notFound } from 'next/navigation';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getBounty } from '@/lib/api';
import type { Bounty, ProviderRouteReceipt } from '@/lib/types';
import BountyDetailPage from './page';

vi.mock('next/navigation', () => ({
  notFound: vi.fn(() => {
    throw new Error('NEXT_NOT_FOUND');
  }),
}));
vi.mock('@/lib/api', () => ({ getBounty: vi.fn() }));

describe('bounty receipt page', () => {
  beforeEach(() => vi.clearAllMocks());

  it('uses the Next 404 boundary when the backend cannot find the bounty', async () => {
    vi.mocked(getBounty).mockResolvedValue({ status: 'not_found' });

    await expect(BountyDetailPage({ params: Promise.resolve({ id: 'missing' }) })).rejects.toThrow(
      'NEXT_NOT_FOUND',
    );
    expect(notFound).toHaveBeenCalledOnce();
  });

  it('keeps API outages in the rendered unavailable state', async () => {
    vi.mocked(getBounty).mockResolvedValue({ status: 'error', error: 'upstream unavailable' });

    await expect(
      BountyDetailPage({ params: Promise.resolve({ id: 'bounty-1' }) }),
    ).resolves.toBeTruthy();
    expect(notFound).not.toHaveBeenCalled();
  });

  it('renders the exact reviewed diff and public provider receipt without a balance', async () => {
    const provider = {
      model: 'bounty-review-model',
      route: 'marketplace',
      providerId: 'provider-11',
      requestId: 'request-12',
      costMicrounits: '90000',
      balanceRemaining: '3910000',
    } as ProviderRouteReceipt;
    const bounty: Bounty = {
      id: 'bounty-reviewed',
      title: 'Repair a bounded regression',
      repository: 'public/tool',
      issueUrl: 'https://github.com/public/tool/issues/1',
      issueNumber: 1,
      amountUsd: 10,
      amountAtomic: '10000000',
      asset: 'SOL',
      state: 'rejected',
      acceptanceCriteria: ['Pass repository checks', 'Pass independent review'],
      review: {
        approved: false,
        reason: 'The patch did not preserve the documented behavior.',
        reviewedAt: '2026-08-23T11:00:00.000Z',
        headSha: 'c'.repeat(40),
        baseSha: 'd'.repeat(40),
        baseRef: 'main',
        diffHash: 'e'.repeat(64),
        provider,
      },
      createdAt: '2026-08-23T09:00:00.000Z',
      updatedAt: '2026-08-23T11:00:00.000Z',
    };
    vi.mocked(getBounty).mockResolvedValue({ status: 'ready', data: bounty });

    const html = renderToStaticMarkup(
      await BountyDetailPage({ params: Promise.resolve({ id: bounty.id }) }),
    );

    expect(html).toContain('Independent review receipt');
    expect(html).toContain('rejected');
    expect(html).toContain('c'.repeat(40));
    expect(html).toContain('d'.repeat(40));
    expect(html).toContain('e'.repeat(64));
    expect(html).toContain('bounty-review-model');
    expect(html).toContain('provider-11');
    expect(html).toContain('request-12');
    expect(html).toContain('90000 microunits');
    expect(html).not.toContain('3910000');
    expect(html).not.toContain('balanceRemaining');
  });
});
