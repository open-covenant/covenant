"use client";

import { useEffect, useState } from "react";
import { solscanTxUrl } from "./explorer";

export type SettlementSig = {
  tx_sig: string;
  slot: number | null;
  settled_at_ms: number;
  intent_text: string | null;
  cluster: string;
};

type SigsPayload = {
  cluster: string;
  sigs: Record<string, SettlementSig>;
};

// Polls the settlement-sigs proxy route every `intervalMs`. Returns a
// `{ get(intent_id), all, cluster }` shape so any page rendering tasks
// can look up the on-chain anchor for the one it's showing.
export function useSettlementSigs(intervalMs = 4000): {
  get: (intentId: string | null | undefined) => SettlementSig | null;
  cluster: string;
  count: number;
} {
  const [payload, setPayload] = useState<SigsPayload>({
    cluster: "devnet",
    sigs: {},
  });

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const res = await fetch("/api/settlement-sigs", { cache: "no-store" });
        if (!res.ok) return;
        const body = (await res.json()) as SigsPayload;
        if (!cancelled && body && typeof body === "object" && body.sigs) {
          setPayload(body);
        }
      } catch {
        // sidecar may be down; the next tick retries
      }
    };
    tick();
    const t = setInterval(tick, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [intervalMs]);

  return {
    get: (intentId) => (intentId ? (payload.sigs[intentId] ?? null) : null),
    cluster: payload.cluster,
    count: Object.keys(payload.sigs).length,
  };
}

// Convenience helper: short-form Solscan link badge string used as the
// children of a tx anchor. Stable shape so the badge looks the same
// across pages.
export function shortSig(sig: string, len = 8): string {
  return sig.length <= len * 2 + 1 ? sig : `${sig.slice(0, len)}…${sig.slice(-len)}`;
}

export function solscanFor(sig: SettlementSig): string {
  return solscanTxUrl(sig.tx_sig, sig.cluster);
}
