'use client';

import { useCallback, useEffect, useState } from 'react';
import { normalizeCsrfToken } from './workbench';

export class WorkbenchRequestError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

export class WorkbenchRequestTimeoutError extends Error {
  constructor() {
    super('The request timed out. Try again.');
  }
}

const workbenchRequestTimeoutMs = 15_000;

type UnauthorizedListener = () => void;
const unauthorizedListeners = new Set<UnauthorizedListener>();

export function onWorkbenchUnauthorized(listener: UnauthorizedListener): () => void {
  unauthorizedListeners.add(listener);
  return () => unauthorizedListeners.delete(listener);
}

export async function workbenchRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetchWithDeadline(
    `/api/mizuki${path}`,
    {
      ...init,
      cache: 'no-store',
      credentials: 'include',
      headers: {
        accept: 'application/json',
        ...init?.headers,
      },
    },
    fetch,
    workbenchRequestTimeoutMs,
  );
  const body = (await response.json().catch(() => ({}))) as T & {
    error?: string;
    reason?: string;
  };
  if (!response.ok) {
    if (response.status === 401) {
      for (const listener of unauthorizedListeners) listener();
    }
    throw new WorkbenchRequestError(
      body.error || body.reason || `Request failed (${response.status})`,
      response.status,
    );
  }
  return body;
}

export async function fetchWithDeadline(
  input: RequestInfo | URL,
  init: RequestInit = {},
  request: typeof fetch = fetch,
  timeoutMs = workbenchRequestTimeoutMs,
): Promise<Response> {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new Error('Request timeout must be a positive number');
  }

  const callerSignal = init.signal;
  if (callerSignal?.aborted) throw abortReason(callerSignal);

  const controller = new AbortController();
  let timedOut = false;
  const abortFromCaller = () => controller.abort(abortReason(callerSignal!));
  callerSignal?.addEventListener('abort', abortFromCaller, { once: true });
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, timeoutMs);

  try {
    return await request(input, { ...init, signal: controller.signal });
  } catch (cause) {
    if (timedOut) throw new WorkbenchRequestTimeoutError();
    throw cause;
  } finally {
    clearTimeout(timer);
    callerSignal?.removeEventListener('abort', abortFromCaller);
  }
}

export async function workbenchMutation<T>(path: string, init: RequestInit): Promise<T> {
  const method = init.method?.toUpperCase();
  if (!method || method === 'GET' || method === 'HEAD') {
    throw new Error('Workbench mutations require an unsafe HTTP method');
  }
  const csrfToken = await sessionCsrfToken();
  return workbenchRequest<T>(path, {
    ...init,
    headers: {
      ...init.headers,
      'x-mizuki-csrf-token': csrfToken,
    },
  });
}

export async function sessionCsrfToken(): Promise<string> {
  return normalizeCsrfToken(await workbenchRequest<unknown>('/v1/auth/csrf'));
}

export async function logoutWorkbench(navigate: () => void | Promise<void>): Promise<void> {
  await workbenchMutation('/v1/auth/logout', { method: 'POST' });
  await navigate();
}

export type WorkbenchResource<T> =
  | { status: 'loading'; refresh: () => void }
  | { status: 'ready'; data: T; refresh: () => void }
  | { status: 'unauthorized'; refresh: () => void }
  | { status: 'error'; error: string; refresh: () => void };

export function useWorkbenchResource<T>(
  path: string,
  parse: (value: unknown) => T,
): WorkbenchResource<T> {
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<
    | { status: 'loading' }
    | { status: 'ready'; data: T }
    | { status: 'unauthorized' }
    | { status: 'error'; error: string }
  >({ status: 'loading' });
  const refresh = useCallback(() => setAttempt((value) => value + 1), []);

  useEffect(() => {
    const controller = new AbortController();
    setState({ status: 'loading' });
    void workbenchRequest<unknown>(path, { signal: controller.signal })
      .then((value) => setState({ status: 'ready', data: parse(value) }))
      .catch((cause) => {
        if (cause instanceof DOMException && cause.name === 'AbortError') return;
        if (cause instanceof WorkbenchRequestError && cause.status === 401) {
          setState({ status: 'unauthorized' });
          return;
        }
        setState({
          status: 'error',
          error:
            cause instanceof Error ? cause.message : 'This information is temporarily unavailable',
        });
      });
    return () => controller.abort();
  }, [attempt, parse, path]);

  return { ...state, refresh };
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException('The operation was aborted', 'AbortError');
}
