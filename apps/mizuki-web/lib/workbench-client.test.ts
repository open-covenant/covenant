import { afterEach, describe, expect, it, vi } from 'vitest';
import { onWorkbenchUnauthorized, workbenchRequest } from './workbench-client';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Workbench session handling', () => {
  it('notifies the shell when any authenticated request returns 401', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: 'not signed in' }, { status: 401 })),
    );
    const expired = vi.fn();
    const unsubscribe = onWorkbenchUnauthorized(expired);

    await expect(workbenchRequest('/v1/account/jobs')).rejects.toMatchObject({
      status: 401,
      message: 'not signed in',
    });
    expect(expired).toHaveBeenCalledOnce();
    unsubscribe();
  });

  it('does not treat a service outage as an expired session', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: 'temporarily unavailable' }, { status: 503 })),
    );
    const expired = vi.fn();
    const unsubscribe = onWorkbenchUnauthorized(expired);

    await expect(workbenchRequest('/v1/account/repositories')).rejects.toMatchObject({
      status: 503,
    });
    expect(expired).not.toHaveBeenCalled();
    unsubscribe();
  });
});
