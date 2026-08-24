import { afterEach, describe, expect, it, vi } from 'vitest';
import { getBounty, getJob } from './api';

vi.mock('server-only', () => ({}));

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
});

describe('receipt detail loading', () => {
  it.each([
    ['job', getJob, '/v1/jobs/missing%2Freceipt'],
    ['bounty', getBounty, '/v1/bounties/missing%2Freceipt'],
  ] as const)(
    'returns not_found only for a backend 404 on %s details',
    async (_name, load, path) => {
      vi.stubEnv('MIZUKI_API_URL', 'https://api.example.com');
      const upstream = vi.fn(async () => Response.json({ error: 'not found' }, { status: 404 }));
      vi.stubGlobal('fetch', upstream);

      await expect(load('missing/receipt')).resolves.toEqual({ status: 'not_found' });
      expect(upstream).toHaveBeenCalledWith(
        `https://api.example.com${path}`,
        expect.objectContaining({ cache: 'no-store' }),
      );
    },
  );

  it.each([
    ['job', getJob],
    ['bounty', getBounty],
  ] as const)('keeps a backend outage as an error for %s details', async (_name, load) => {
    vi.stubEnv('MIZUKI_API_URL', 'https://api.example.com');
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: 'unavailable' }, { status: 503 })),
    );

    await expect(load('receipt-1')).resolves.toEqual({
      status: 'error',
      error: 'Live records are temporarily unavailable',
    });
  });
});
