"use client";

import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

// Slot pattern: pages call `useRightRail(<jsx>)` from inside their own
// render to push content into the Shell's optional right context column.
// Shell reads via `useRightRailContent()`; when null, Shell stays in its
// two-column layout. Content closes over the page's live state (polled
// data, handlers) because pages pass JSX every render — the slot is just
// a re-parented subtree, not a snapshot.

type Ctx = {
  node: ReactNode | null;
  setNode: (node: ReactNode | null) => void;
};

const RightRailContext = createContext<Ctx | null>(null);

export function RightRailProvider({ children }: { children: ReactNode }) {
  const [node, setNode] = useState<ReactNode | null>(null);
  return (
    <RightRailContext.Provider value={{ node, setNode }}>
      {children}
    </RightRailContext.Provider>
  );
}

export function useRightRailContent(): ReactNode | null {
  const ctx = useContext(RightRailContext);
  return ctx ? ctx.node : null;
}

/**
 * Mount JSX into Shell's right rail. Effect re-runs on every commit so
 * the slot reflects the latest page state, and clears on unmount so the
 * rail collapses when navigating to a page that doesn't mount one.
 */
export function useRightRail(node: ReactNode | null): void {
  const ctx = useContext(RightRailContext);
  useEffect(() => {
    if (!ctx) return;
    ctx.setNode(node);
    return () => {
      ctx.setNode(null);
    };
  });
}
