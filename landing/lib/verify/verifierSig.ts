import { createPublicKey, verify as edVerify } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

// Anchor 4 — verifier-refuter signature. A separately-keyed ed25519 verifier
// signs the audit root into attestations/<sha>.verifier.sig. Presence alone is
// not sufficient: the light stays yellow until the signature is checked against
// the published verifier pubkey, turns red if it fails to verify or the
// verifier refuted the run, and is green only on a verified non-refutation.
export function checkAnchor4VerifierSig(repoRoot: string, sha: string): Witness {
  const sigPath = join(repoRoot, "attestations", `${sha}.verifier.sig`);
  if (!existsSync(sigPath)) {
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "yellow",
      detail:
        "No verifier signature published for this commit yet. A separately-keyed ed25519 verifier signs the audit root; until then this light reads yellow.",
    };
  }
  try {
    const att = JSON.parse(readFileSync(join(repoRoot, "attestations", `${sha}.json`), "utf8")) as {
      audit_root_hex?: string;
      verdict?: string;
      domain?: string;
    };
    const pubkeyPath = join(repoRoot, "landing", "public", "witness", "verifier-pubkey.txt");
    if (!att.audit_root_hex || !existsSync(pubkeyPath)) {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "yellow",
        detail: "Verifier signature present but the published pubkey or audit root is missing.",
      };
    }
    const pubkey = readFileSync(pubkeyPath, "utf8").trim();
    const sig = readFileSync(sigPath, "utf8").trim();
    const domain = att.domain || "covenant.witness.v1";
    const message = Buffer.from(`${domain}\n${att.audit_root_hex}`, "utf8");
    const key = createPublicKey({ format: "jwk", key: { kty: "OKP", crv: "Ed25519", x: pubkey } });
    if (!edVerify(null, message, key, Buffer.from(sig, "base64url"))) {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "red",
        detail: "Verifier signature did not verify against the published verifier pubkey.",
      };
    }
    if (att.verdict === "refute") {
      return {
        key: "verifier_sig",
        label: "Verifier-Refuter signature",
        state: "red",
        detail: `Verifier refuted this run (signed by ${pubkey.slice(0, 12)}…): a signed action causally followed untrusted on-chain input.`,
      };
    }
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "green",
      detail: `Audit root signed by an independent verifier (${pubkey.slice(0, 12)}…), no refutation. Check it yourself against landing/public/witness/verifier-pubkey.txt.`,
    };
  } catch {
    return {
      key: "verifier_sig",
      label: "Verifier-Refuter signature",
      state: "red",
      detail: "Verifier signature or pubkey unreadable.",
    };
  }
}
