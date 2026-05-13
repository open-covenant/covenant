"use client";

import { useSyncExternalStore } from "react";

const KEY = "covenant.developer_mode";
const listeners = new Set<() => void>();

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

function getSnapshot(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(KEY) === "1";
}

function getServerSnapshot(): boolean {
  return false;
}

function set(value: boolean): void {
  if (typeof window === "undefined") return;
  if (value) window.localStorage.setItem(KEY, "1");
  else window.localStorage.removeItem(KEY);
  for (const cb of listeners) cb();
}

export function useDeveloperMode(): [boolean, (next: boolean) => void] {
  const value = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
  return [value, set];
}
