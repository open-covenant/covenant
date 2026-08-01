import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

// Anchor 3 legacy manifest reader. It compares two repository files but does
// not fetch or decode the claimed PDA, transaction, slot, or finality. A
// structurally consistent manifest is therefore yellow, never chain-verified.
export function checkAnchor3Solana(repoRoot: string, sha: string): Witness {
  const manifestPath = join(repoRoot, "landing", "public", "witness", "settlement", `${sha}.json`);
  if (!existsSync(manifestPath)) {
    return {
      key: "solana_anchor",
      label: "Published settlement report",
      state: "yellow",
      detail: "No settlement manifest is published for this commit.",
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
        label: "Published settlement report",
        state: "red",
        detail: "Settlement batch present but its committed root does not match the run's audit root.",
        drillHref,
      };
    }
    return {
      key: "solana_anchor",
      label: "Published settlement report",
      state: "yellow",
      detail: `The mutable manifest reports batch ${m.batch_pda.slice(0, 12)}… on ${cluster}, transaction ${m.tx.slice(0, 12)}…, and the same root as the run file at slot ${m.slot ?? "?"}. This page has not fetched or decoded the PDA or established transaction finality.`,
      drillHref,
    };
  } catch {
    return {
      key: "solana_anchor",
      label: "Published settlement report",
      state: "red",
      detail: "Settlement manifest unreadable.",
    };
  }
}
