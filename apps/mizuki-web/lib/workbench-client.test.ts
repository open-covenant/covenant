import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  fetchWithDeadline,
  logoutWorkbench,
  onWorkbenchUnauthorized,
  workbenchRequest,
  WorkbenchRequestTimeoutError,
} from './workbench-client';

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('bounded Workbench requests', () => {
  it('propagates a caller abort through the combined request signal', async () => {
    const caller = new AbortController();
    const request = vi.fn(
      async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(init.signal?.reason), {
            once: true,
          });
        }),
    );

    const pending = fetchWithDeadline('/resource', { signal: caller.signal }, request, 10_000);
    caller.abort();

    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    expect(request.mock.calls[0]?.[1]?.signal).not.toBe(caller.signal);
  });

  it('ends a stalled request at its deadline', async () => {
    vi.useFakeTimers();
    const request = vi.fn(
      async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => reject(init.signal?.reason), {
            once: true,
          });
        }),
    );
    const pending = fetchWithDeadline('/resource', {}, request, 1_000);
    const assertion = expect(pending).rejects.toBeInstanceOf(WorkbenchRequestTimeoutError);

    await vi.advanceTimersByTimeAsync(1_000);
    await assertion;
  });
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
    const request = vi
      .fn()
      .mockResolvedValueOnce(Response.json({ csrfToken: 'a'.repeat(43) }))
      .mockResolvedValueOnce(Response.json({ ok: true }));
    vi.stubGlobal('fetch', request);
    const navigate = vi.fn();

    await logoutWorkbench(navigate);

    expect(request).toHaveBeenNthCalledWith(
      1,
      '/api/mizuki/v1/auth/csrf',
      expect.objectContaining({ credentials: 'include' }),
    );
    expect(request).toHaveBeenNthCalledWith(
      2,
      '/api/mizuki/v1/auth/logout',
      expect.objectContaining({
        method: 'POST',
        credentials: 'include',
        headers: expect.objectContaining({ 'x-mizuki-csrf-token': 'a'.repeat(43) }),
      }),
    );
    expect(navigate).toHaveBeenCalledOnce();
  });

  it('rejects unsafe requests that bypass the mutation helper', async () => {
    const request = vi.fn();
    vi.stubGlobal('fetch', request);

    await expect(workbenchRequest('/v1/preflights', { method: 'POST' })).rejects.toThrow(
      'Unsafe Workbench requests must use workbenchMutation',
    );
    expect(request).not.toHaveBeenCalled();
  });
});
