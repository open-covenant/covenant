import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

// Anchor 1 legacy manifest reader. This code does not query Solana, decode the
// transaction, or authenticate the claimed authority, so a manifest can only
// produce a yellow publisher-report state, never a chain-verified green.
export function checkAnchor1CommitMemo(repoRoot: string, sha: string): Witness {
  const memoManifest = join(repoRoot, "landing", "public", "witness", "memo", `${sha}.json`);
  if (!existsSync(memoManifest)) {
    return {
      key: "rekor",
      label: "Published memo report",
      state: "yellow",
      detail: "No memo report is published for this commit.",
      badge: { text: "Anchor not yet live", tone: "yellow" },
    };
  }
  try {
    const parsed = JSON.parse(readFileSync(memoManifest, "utf8")) as {
      tx?: string;
      verified?: boolean;
      slot?: number;
      authority?: string;
      cluster?: "devnet" | "mainnet";
    };
    const cluster = parsed.cluster ?? "devnet";
    const solscan = parsed.tx
      ? `https://solscan.io/tx/${parsed.tx}${cluster === "devnet" ? "?cluster=devnet" : ""}`
      : undefined;
    if (!parsed.tx) {
      return {
        key: "rekor",
        label: "Published memo report",
        state: "red",
        detail: "Memo manifest is present but carries no transaction id.",
        drillHref: solscan,
      };
    }
    return {
      key: "rekor",
      label: "Published memo report",
      state: "yellow",
      detail: `The mutable manifest reports memo tx ${parsed.tx.slice(0, 16)}… at slot ${parsed.slot ?? "?"} (${cluster}) and marks it ${parsed.verified ? "verified" : "unverified"}. This page has not queried or decoded the transaction or authenticated ${parsed.authority || "the claimed authority"}.`,
      drillHref: solscan,
    };
  } catch {
    return {
      key: "rekor",
      label: "Published memo report",
      state: "red",
      detail: "Memo manifest unreadable — investigate landing/public/witness/memo/<sha>.json.",
    };
  }
}
