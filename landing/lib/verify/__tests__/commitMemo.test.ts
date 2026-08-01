import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { checkAnchor1CommitMemo } from "../commitMemo";

// checkAnchor1CommitMemo reads the per-commit Solana memo manifest
// (landing/public/witness/memo/<sha>.json) for the public verify surface. It is
// It never treats mutable manifest assertions as live chain verification.
describe("checkAnchor1CommitMemo", () => {
  let root: string;
  const dir = () => join(root, "landing", "public", "witness", "memo");
  const write = (sha: string, body: unknown) => {
    mkdirSync(dir(), { recursive: true });
    writeFileSync(join(dir(), `${sha}.json`), typeof body === "string" ? body : JSON.stringify(body));
  };
  beforeAll(() => {
    root = mkdtempSync(join(tmpdir(), "cov-memo-"));
  });
  afterAll(() => rmSync(root, { recursive: true, force: true }));

  it("stays yellow when no memo manifest exists for the commit", () => {
    const w = checkAnchor1CommitMemo(root, "absent");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("No memo report is published");
    expect(w.badge).toEqual({ text: "Anchor not yet live", tone: "yellow" });
  });

  it("reports an unverified manifest as yellow publisher data", () => {
    write("unverified", { tx: "TX1", verified: false, cluster: "devnet" });
    const w = checkAnchor1CommitMemo(root, "unverified");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("marks it unverified");
    expect(w.detail).toContain("has not queried or decoded");
  });

  it("turns red when the manifest is verified but carries no tx", () => {
    write("notx", { verified: true, cluster: "devnet" });
    const w = checkAnchor1CommitMemo(root, "notx");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("carries no transaction id");
  });

  it("reports a verified manifest assertion as yellow, never chain-verified", () => {
    write("ok", {
      tx: "TXSIGNATURE0000000",
      verified: true,
      slot: 7,
      authority: "AUTHpub",
      cluster: "devnet",
    });
    const w = checkAnchor1CommitMemo(root, "ok");
    expect(w).toMatchObject({
      key: "rekor",
      label: "Published memo report",
      state: "yellow",
    });
    expect(w.detail).toContain("marks it verified");
    expect(w.detail).toContain("AUTHpub");
  });

  it("appends the devnet cluster query to the drill link by default", () => {
    write("devdefault", { tx: "TXDEV", verified: true });
    expect(checkAnchor1CommitMemo(root, "devdefault").drillHref).toBe("https://solscan.io/tx/TXDEV?cluster=devnet");
  });

  it("omits the cluster query for a mainnet tx so the drill link is not misrouted", () => {
    write("mainnet", { tx: "TXMAIN", verified: true, cluster: "mainnet" });
    expect(checkAnchor1CommitMemo(root, "mainnet").drillHref).toBe("https://solscan.io/tx/TXMAIN");
  });

  it("defaults the authority label when the manifest omits it", () => {
    write("noauth", { tx: "TXNOAUTH", verified: true });
    expect(checkAnchor1CommitMemo(root, "noauth").detail).toContain(
      "claimed authority",
    );
  });

  it("turns red when the memo manifest is unreadable", () => {
    write("badjson", "{not valid json");
    const w = checkAnchor1CommitMemo(root, "badjson");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("unreadable");
  });
});
