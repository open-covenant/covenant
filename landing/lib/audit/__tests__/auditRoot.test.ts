import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { recomputeAuditRoot } from "../auditRoot";

// Golden roots computed independently (Python sha256, not this module) for the
// canonical algorithm: genesis = 64 hex zeros, per step event = sha256(line),
// chain = sha256(prev + "\n" + event). These literals also equal what the Rust
// verifier (agent-os/crates/covenant-audit chain_hash + ZERO_CHAIN_HASH)
// produces, so they pin the cross-language witness contract — drift in either
// direction reddens here or in covenant-audit's chain_hash test.
const GENESIS = "0".repeat(64);
const ROOT_A = "8b40f5317d2d340480a329f4393307985fc034b663e1e76acc5ce5b3fa407be8";
const ROOT_A_B = "928de12d1dc7278f66368646b2712f59b89c0261b83d289ff5f8cd0b78cebe6f";

describe("recomputeAuditRoot", () => {
  it("returns the genesis 64-zero root for an empty chain", () => {
    expect(recomputeAuditRoot([])).toBe(GENESIS);
  });

  it("matches the independently-computed root for a single-step chain", () => {
    expect(recomputeAuditRoot(["a"])).toBe(ROOT_A);
  });

  it("folds successive steps into the chain accumulator", () => {
    expect(recomputeAuditRoot(["a", "b"])).toBe(ROOT_A_B);
  });

  it("is order sensitive — reordering the same lines changes the root", () => {
    expect(recomputeAuditRoot(["b", "a"])).not.toBe(ROOT_A_B);
  });

  it("seeds from the prior root — a one-step prefix differs from its two-step extension", () => {
    expect(recomputeAuditRoot(["a"])).not.toBe(recomputeAuditRoot(["a", "b"]));
  });

  it("is pure — equal inputs yield equal roots", () => {
    expect(recomputeAuditRoot(["a", "b"])).toBe(recomputeAuditRoot(["a", "b"]));
  });
});

// The published witness this route serves: replaying its canonical steps must
// reproduce the committed audit_root_hex byte-for-byte, otherwise the public
// /api/verify proof would silently disagree with the on-disk attestation.
describe("recomputeAuditRoot against the committed attestation", () => {
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");
  const sha = "1682e27f0a4a51c73a9bfe552a6144944dbc75d9";
  const attestationPath = resolve(repoRoot, "attestations", `${sha}.json`);

  it("reproduces the committed root from the published steps", () => {
    expect(existsSync(attestationPath)).toBe(true);
    const att = JSON.parse(readFileSync(attestationPath, "utf8")) as {
      audit_root_hex: string;
      steps: string[];
    };
    expect(att.steps.length).toBeGreaterThan(1);
    expect(recomputeAuditRoot(att.steps)).toBe(att.audit_root_hex);
  });
});
