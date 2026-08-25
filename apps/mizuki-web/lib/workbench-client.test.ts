import { afterEach, describe, expect, it, vi } from 'vitest';
import { logoutWorkbench, onWorkbenchUnauthorized, workbenchRequest } from './workbench-client';

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

  it('keeps the current session visible when logout fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => Response.json({ error: 'temporarily unavailable' }, { status: 503 })),
    );
    const navigate = vi.fn();

    await expect(logoutWorkbench(navigate)).rejects.toMatchObject({ status: 503 });
    expect(navigate).not.toHaveBeenCalled();
  });

  it('navigates only after logout succeeds', async () => {
    const request = vi.fn(async () => Response.json({ ok: true }));
    vi.stubGlobal('fetch', request);
    const navigate = vi.fn();

    await logoutWorkbench(navigate);

    expect(request).toHaveBeenCalledWith(
      '/api/mizuki/v1/auth/logout',
      expect.objectContaining({ method: 'POST', credentials: 'include' }),
    );
    expect(navigate).toHaveBeenCalledOnce();
  });
});
