'use client';

import { useCallback, useEffect, useState } from 'react';

export class WorkbenchRequestError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

type UnauthorizedListener = () => void;
const unauthorizedListeners = new Set<UnauthorizedListener>();

export function onWorkbenchUnauthorized(listener: UnauthorizedListener): () => void {
  unauthorizedListeners.add(listener);
  return () => unauthorizedListeners.delete(listener);
}

export async function workbenchRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/mizuki${path}`, {
    ...init,
    cache: 'no-store',
    credentials: 'include',
    headers: {
      accept: 'application/json',
      ...init?.headers,
    },
  });
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
