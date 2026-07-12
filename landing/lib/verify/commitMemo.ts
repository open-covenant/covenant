import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

// Anchor 1 — per-commit Solana Memo tx, signed by the operator authority, with
// payload covenant-commit-v1:<sha>:<audit_root_hex>:<unix_ms>. Reads the
// recorded tx from landing/public/witness/memo/<sha>.json and confirms it. The
// light is green only when the manifest marks the memo verified and carries a
// tx; an unverified or tx-less manifest reads red, and an unreadable manifest
// fails closed to red rather than a false green.
export function checkAnchor1CommitMemo(repoRoot: string, sha: string): Witness {
  const memoManifest = join(repoRoot, "landing", "public", "witness", "memo", `${sha}.json`);
  if (!existsSync(memoManifest)) {
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "yellow",
      detail:
        "No memo anchor published for this commit yet. When the anchor daemon posts a memo tx (payload covenant-commit-v1:<sha>:<audit_root_hex>:<ts>, signed by the operator authority), this light verifies it.",
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
    if (!parsed.verified || !parsed.tx) {
      return {
        key: "rekor",
        label: "Solana commit memo",
        state: "red",
        detail: `Memo tx ${parsed.tx || "missing"} did not verify against the operator authority pubkey.`,
        drillHref: solscan,
      };
    }
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "green",
      detail: `Memo tx ${parsed.tx.slice(0, 16)}… signed by ${parsed.authority || "operator authority"} at slot ${parsed.slot ?? "?"} (${cluster}).`,
      drillHref: solscan,
    };
  } catch {
    return {
      key: "rekor",
      label: "Solana commit memo",
      state: "red",
      detail: "Memo manifest unreadable — investigate landing/public/witness/memo/<sha>.json.",
    };
  }
}
