import { generateKeyPairSync, sign as edSign } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { checkAnchor4VerifierSig } from "../verifierSig";

// checkAnchor4VerifierSig is Anchor 4 of the public witness verify surface: a
// separately-keyed ed25519 verifier signs the audit root into
// attestations/<sha>.verifier.sig, and the light is green only on a signature
// that verifies against the published pubkey with no refutation. These tests use
// a real keypair so the fail-closed arms — forged signature, refuted run, missing
// pubkey or audit root, unreadable attestation — can never silently weaken into a
// false green.
describe("checkAnchor4VerifierSig", () => {
  const ROOT_HEX = "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";
  const roots: string[] = [];
  const mkRoot = () => {
    const r = mkdtempSync(join(tmpdir(), "cov-vsig-"));
    roots.push(r);
    return r;
  };
  afterAll(() => roots.forEach((r) => rmSync(r, { recursive: true, force: true })));

  const keypair = () => {
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    const x = (publicKey.export({ format: "jwk" }) as { x: string }).x;
    return { x, privateKey };
  };
  const signRoot = (privateKey: ReturnType<typeof keypair>["privateKey"], domain: string, root: string) =>
    edSign(null, Buffer.from(`${domain}\n${root}`, "utf8"), privateKey).toString("base64url");

  const attDir = (root: string) => join(root, "attestations");
  const writeSig = (root: string, sha: string, sig: string) => {
    mkdirSync(attDir(root), { recursive: true });
    writeFileSync(join(attDir(root), `${sha}.verifier.sig`), sig);
  };
  const writeAtt = (root: string, sha: string, body: unknown) => {
    mkdirSync(attDir(root), { recursive: true });
    writeFileSync(join(attDir(root), `${sha}.json`), typeof body === "string" ? body : JSON.stringify(body));
  };
  const writePubkey = (root: string, x: string) => {
    const dir = join(root, "landing", "public", "witness");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "verifier-pubkey.txt"), x);
  };

  it("stays yellow when no verifier signature is published for the commit", () => {
    const w = checkAnchor4VerifierSig(mkRoot(), "absent");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("No verifier signature published");
  });

  it("stays yellow when the audit root is missing from the attestation", () => {
    const root = mkRoot();
    const { x } = keypair();
    writeSig(root, "noroot", "sig");
    writeAtt(root, "noroot", { verdict: "attest" });
    writePubkey(root, x);
    const w = checkAnchor4VerifierSig(root, "noroot");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("audit root is missing");
  });

  it("stays yellow when the published verifier pubkey is missing", () => {
    const root = mkRoot();
    writeSig(root, "nopub", "sig");
    writeAtt(root, "nopub", { audit_root_hex: ROOT_HEX });
    const w = checkAnchor4VerifierSig(root, "nopub");
    expect(w.state).toBe("yellow");
    expect(w.detail).toContain("audit root is missing");
  });

  it("turns red when the signature does not verify against the published pubkey", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeAtt(root, "forged", { audit_root_hex: ROOT_HEX });
    writePubkey(root, x);
    writeSig(root, "forged", signRoot(privateKey, "covenant.witness.v1", "a-different-root"));
    const w = checkAnchor4VerifierSig(root, "forged");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("did not verify");
  });

  it("turns red when the verifier refuted the run even with a valid signature", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeAtt(root, "refuted", { audit_root_hex: ROOT_HEX, verdict: "refute" });
    writePubkey(root, x);
    writeSig(root, "refuted", signRoot(privateKey, "covenant.witness.v1", ROOT_HEX));
    const w = checkAnchor4VerifierSig(root, "refuted");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("refuted");
    expect(w.detail).toContain(x.slice(0, 12));
  });

  it("turns green for a valid signature with no refutation", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeAtt(root, "ok", { audit_root_hex: ROOT_HEX, verdict: "attest" });
    writePubkey(root, x);
    writeSig(root, "ok", signRoot(privateKey, "covenant.witness.v1", ROOT_HEX));
    const w = checkAnchor4VerifierSig(root, "ok");
    expect(w).toMatchObject({ key: "verifier_sig", label: "Verifier-Refuter signature", state: "green" });
    expect(w.detail).toContain("signed by an independent verifier");
    expect(w.detail).toContain(x.slice(0, 12));
  });

  it("defaults the signing domain to covenant.witness.v1 when the attestation omits it", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeAtt(root, "defdomain", { audit_root_hex: ROOT_HEX });
    writePubkey(root, x);
    writeSig(root, "defdomain", signRoot(privateKey, "covenant.witness.v1", ROOT_HEX));
    expect(checkAnchor4VerifierSig(root, "defdomain").state).toBe("green");
  });

  it("uses the attestation domain when present so a domain mismatch fails closed", () => {
    const root = mkRoot();
    const { x, privateKey } = keypair();
    writeAtt(root, "altdomain", { audit_root_hex: ROOT_HEX, domain: "covenant.witness.v9" });
    writePubkey(root, x);
    writeSig(root, "altdomain", signRoot(privateKey, "covenant.witness.v9", ROOT_HEX));
    expect(checkAnchor4VerifierSig(root, "altdomain").state).toBe("green");
  });

  it("turns red when the attestation JSON is unreadable", () => {
    const root = mkRoot();
    const { x } = keypair();
    writeSig(root, "badjson", "sig");
    writeAtt(root, "badjson", "{not valid json");
    writePubkey(root, x);
    const w = checkAnchor4VerifierSig(root, "badjson");
    expect(w.state).toBe("red");
    expect(w.detail).toContain("unreadable");
  });
});
