import { notFound } from 'next/navigation';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getJob } from '@/lib/api';
import JobPage from './page';

vi.mock('next/navigation', () => ({
  notFound: vi.fn(() => {
    throw new Error('NEXT_NOT_FOUND');
  }),
}));
vi.mock('@/lib/api', () => ({ getJob: vi.fn() }));

describe('job receipt page', () => {
  beforeEach(() => vi.clearAllMocks());

  it('uses the Next 404 boundary when the backend cannot find the job', async () => {
    vi.mocked(getJob).mockResolvedValue({ status: 'not_found' });

    await expect(JobPage({ params: Promise.resolve({ id: 'missing' }) })).rejects.toThrow(
      'NEXT_NOT_FOUND',
    );
    expect(notFound).toHaveBeenCalledOnce();
  });

  it('keeps API outages in the rendered unavailable state', async () => {
    vi.mocked(getJob).mockResolvedValue({ status: 'error', error: 'upstream unavailable' });

    await expect(JobPage({ params: Promise.resolve({ id: 'job-1' }) })).resolves.toBeTruthy();
    expect(notFound).not.toHaveBeenCalled();
  });
});
