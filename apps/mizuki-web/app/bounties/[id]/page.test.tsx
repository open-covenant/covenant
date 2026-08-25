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
  useRouter: vi.fn(() => ({ push: vi.fn(), refresh: vi.fn() })),
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

  it('renders bounded GitHub errors on a public bounty return path', async () => {
    const bounty: Bounty = {
      id: 'bounty-open',
      title: 'Repair a bounded regression',
      repository: 'public/tool',
      issueUrl: 'https://github.com/public/tool/issues/1',
      issueNumber: 1,
      amountUsd: 10,
      amountAtomic: '10000000',
      asset: 'SOL',
      state: 'open',
      escrowTransaction: 'bounty-sol-funding',
      acceptanceCriteria: ['Pass repository checks'],
      createdAt: '2026-08-23T09:00:00.000Z',
      updatedAt: '2026-08-23T11:00:00.000Z',
    };
    vi.mocked(getBounty).mockResolvedValue({ status: 'ready', data: bounty });

    const html = renderToStaticMarkup(
      await BountyDetailPage({
        params: Promise.resolve({ id: bounty.id }),
        searchParams: Promise.resolve({ auth_error: 'replayed' }),
      }),
    );

    expect(html).toContain('This GitHub sign-in request was already used.');
    expect(html).toContain('role="alert"');

    const unsafeHtml = renderToStaticMarkup(
      await BountyDetailPage({
        params: Promise.resolve({ id: bounty.id }),
        searchParams: Promise.resolve({ auth_error: 'private database detail' }),
      }),
    );
    expect(unsafeHtml).not.toContain('private database detail');
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
      state: 'refunded',
      customerRefundTransaction: 'customer-usdc-refund',
      escrowReturnTransaction: 'bounty-sol-return',
      acceptanceCriteria: ['Pass repository checks', 'Pass a separate AI review'],
      review: {
        approved: false,
        reason:
          'The separate AI review did not approve the patch against the issue scope and repository checks.',
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

    expect(html).toContain('Separate AI review record');
    expect(html).toContain('Not approved');
    expect(html).toContain('c'.repeat(40));
    expect(html).toContain('d'.repeat(40));
    expect(html).toContain('e'.repeat(64));
    expect(html).toContain('bounty-review-model');
    expect(html).toContain('provider-11');
    expect(html).toContain('request-12');
    expect(html).toContain('$0.09');
    expect(html).toContain('Returned from escrow');
    expect(html).toContain('Customer refund');
    expect(html).toContain('customer-usdc-refund');
    expect(html).toContain('Escrow return');
    expect(html).toContain('bounty-sol-return');
    expect(html).toContain('Bounty outcome');
    expect(html).not.toContain('Claim this work');
    expect(html).not.toContain('Sign in with GitHub');
    expect(html).not.toContain('3910000');
    expect(html).not.toContain('balanceRemaining');
  });
});
