"use client";

import { useCallback, useEffect, useRef, useState } from "react";

type FetchFn<T> = () => Promise<T>;

export type PollState<T> = {
  data: T | null;
  error: string | null;
  lastSyncMs: number | null;
  refresh: () => Promise<void>;
};

export function usePoll<T>(fetcher: FetchFn<T>, intervalMs = 3000): PollState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastSyncMs, setLastSyncMs] = useState<number | null>(null);
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  const refresh = useCallback(async () => {
    try {
      const result = await fetcherRef.current();
      setData(result);
      setError(null);
      setLastSyncMs(Date.now());
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(message.includes("Failed to fetch") ? "daemon unavailable at 127.0.0.1:8421" : message);
    }
  }, []);

  useEffect(() => {
    refresh();
    if (intervalMs <= 0) return;
    const t = setInterval(refresh, intervalMs);
    return () => clearInterval(t);
  }, [refresh, intervalMs]);

  return { data, error, lastSyncMs, refresh };
}
