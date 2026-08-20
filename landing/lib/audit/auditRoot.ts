import { createHash } from "node:crypto";

// Recompute the audit-chain root from a run's canonical event lines, exactly as
// the daemon's Rust verifier does (agent-os/crates/covenant-audit/src/lib.rs:
// chain_hash + the ZERO_CHAIN_HASH genesis): event_hash = sha256(line), then
// chain = sha256(prev + "\n" + event_hash), seeded with 64 hex zeros. The
// published steps ARE those canonical lines, so this is an independent check of
// a published audit root, not a trust. Any drift from the Rust algorithm makes
// the public witness silently wrong, so the golden vectors in
// __tests__/auditRoot.test.ts pin the cross-language contract.
export function recomputeAuditRoot(steps: string[]): string {
  const sha256 = (b: Buffer) => createHash("sha256").update(b).digest("hex");
  let prev = "0".repeat(64);
  for (const line of steps) {
    const eventHash = sha256(Buffer.from(line, "utf8"));
    prev = sha256(Buffer.from(`${prev}\n${eventHash}`, "utf8"));
  }
  return prev;
}
