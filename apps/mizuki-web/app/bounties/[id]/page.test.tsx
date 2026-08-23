import { notFound } from 'next/navigation';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getBounty } from '@/lib/api';
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
});
