import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { checkAnchor3Solana } from "../settlement";

// checkAnchor3Solana reads the settlement-batch manifest
// (landing/public/witness/settlement/<sha>.json) for the public verify surface
// and is green only when a confirmed batch carries a PDA, a tx, and a merkle_root
// that equals the run's audit_root_hex read from attestations/<sha>.json. These
// tests pair manifest and attestation fixtures so the on-chain-root-vs-run-root
// cross-check and the malformed-manifest guards can never silently weaken into a
// false green.
describe("checkAnchor3Solana", () => {
  let root: string;
  const ROOT = "9f8e7d6c5b4a39281706f5e4d3c2b1a0";
  const OTHER = "00112233445566778899aabbccddeeff";
  const manifestDir = () => join(root, "landing", "public", "witness", "settlement");
  const writeManifest = (sha: string, body: unknown) => {
    mkdirSync(manifestDir(), { recursive: true });
    writeFileSync(join(manifestDir(), `${sha}.json`), typeof body === "string" ? body : JSON.stringify(body));
  };
  const writeAtt = (sha: string, auditRoot: string) => {
    mkdirSync(join(root, "attestations"), { recursive: true });
    writeFileSync(join(root, "attestations", `${sha}.json`), JSON.stringify({ audit_root_hex: auditRoot }));
  };
  const full = (extra: Record<string, unknown> = {}) => ({ batch_pda: "PDA1", tx: "TX1", merkle_root: ROOT, slot: 9, ...extra });
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-settle-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("stays yellow when no settlement manifest exists for the commit", () => {
    const w = checkAnchor3Solana(root, "absent");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("No settlement batch anchored");
    expect(w.drillHref).toContain("cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y");
  });

  it("turns red when the manifest carries no batch PDA", () => {
    writeManifest("nopda", { tx: "TX1", merkle_root: ROOT });
    writeAtt("nopda", ROOT);
    const w = checkAnchor3Solana(root, "nopda");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("does not match the run's audit root");
  });

  it("turns red when the manifest carries no tx", () => {
    writeManifest("notx", { batch_pda: "PDA1", merkle_root: ROOT });
    writeAtt("notx", ROOT);
    const w = checkAnchor3Solana(root, "notx");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("does not match the run's audit root");
  });

  it("turns red when the manifest carries no merkle root", () => {
    writeManifest("noroot", { batch_pda: "PDA1", tx: "TX1" });
    const w = checkAnchor3Solana(root, "noroot");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("does not match the run's audit root");
  });

  it("turns red when the committed root does not match the run's audit root", () => {
    writeManifest("mismatch", full({ merkle_root: ROOT }));
    writeAtt("mismatch", OTHER);
    const w = checkAnchor3Solana(root, "mismatch");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("does not match the run's audit root");
  });

  it("turns green when the committed root matches the run's audit root", () => {
    writeManifest("ok", full());
    writeAtt("ok", ROOT);
    const w = checkAnchor3Solana(root, "ok");
    expect(w).toMatchObject({ key: "solana_anchor", label: "Solana settlement anchor", state: "green" });
    expect(w.detail).toContain("commits audit root");
    expect(w.drillHref).toBe("https://solscan.io/account/PDA1?cluster=devnet");
  });

  it("routes the drill link to the manifest cluster when not devnet", () => {
    writeManifest("mainnet", full({ cluster: "mainnet" }));
    writeAtt("mainnet", ROOT);
    expect(checkAnchor3Solana(root, "mainnet").drillHref).toBe("https://solscan.io/account/PDA1?cluster=mainnet");
  });

  it("turns red when the settlement manifest is unreadable", () => {
    writeManifest("badjson", "{not valid json");
    const w = checkAnchor3Solana(root, "badjson");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("unreadable");
  });
});
