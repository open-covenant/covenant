import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

// Anchor 3 — settlement-program anchor. Looks for a ReceiptBatch PDA on the
// settlement program holding this commit's Merkle leaf in a confirmed batch. The
// light is green only when the batch carries a PDA, a tx, and a merkle_root that
// equals the run's audit_root_hex; a batch whose committed root does not match
// the run, or an unreadable manifest, fails closed to red.
export function checkAnchor3Solana(repoRoot: string, sha: string): Witness {
  const manifestPath = join(repoRoot, "landing", "public", "witness", "settlement", `${sha}.json`);
  if (!existsSync(manifestPath)) {
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "yellow",
      detail:
        "No settlement batch anchored for this commit yet. A receipt batch on cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y commits the audit root on-chain; until it lands this light reads yellow.",
      drillHref: `https://solscan.io/account/cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y?cluster=devnet`,
    };
  }
  try {
    const m = JSON.parse(readFileSync(manifestPath, "utf8")) as {
      tx?: string;
      batch_pda?: string;
      merkle_root?: string;
      slot?: number;
      cluster?: string;
    };
    const attPath = join(repoRoot, "attestations", `${sha}.json`);
    const auditRoot = existsSync(attPath)
      ? (JSON.parse(readFileSync(attPath, "utf8")) as { audit_root_hex?: string }).audit_root_hex
      : undefined;
    const cluster = m.cluster ?? "devnet";
    const drillHref = m.batch_pda
      ? `https://solscan.io/account/${m.batch_pda}?cluster=${cluster}`
      : undefined;
    if (!m.batch_pda || !m.tx || !m.merkle_root || m.merkle_root !== auditRoot) {
      return {
        key: "solana_anchor",
        label: "Solana settlement anchor",
        state: "red",
        detail: "Settlement batch present but its committed root does not match the run's audit root.",
        drillHref,
      };
    }
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "green",
      detail: `Receipt batch ${m.batch_pda.slice(0, 12)}… on devnet commits audit root ${m.merkle_root.slice(0, 12)}… at slot ${m.slot ?? "?"}. Decode the PDA on-chain to check.`,
      drillHref,
    };
  } catch {
    return {
      key: "solana_anchor",
      label: "Solana settlement anchor",
      state: "red",
      detail: "Settlement manifest unreadable.",
    };
  }
}
