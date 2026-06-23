// Pure, side-effect-free extraction of verify-run.mjs's audit chain-root
// fold and W011 refutation scan. Importing this module does no I/O and no
// keygen, so it is unit-testable offline.
import { createHash } from "node:crypto";

// All-zero genesis seed, identical to the daemon's covenant-audit
// ZERO_CHAIN_HASH (agent-os/crates/covenant-audit/src/lib.rs:637).
export const ZERO_CHAIN_HASH = "0".repeat(64);
export const WITNESS_DOMAIN = "covenant.witness.v1";

const sha256hex = (buf) => createHash("sha256").update(buf).digest("hex");

// Recompute the tamper-evident chain root from raw event lines, exactly as
// the daemon's covenant-audit chain folds them (chain_hash lib.rs:649,
// build_chain_entries lib.rs:671). event_hash = sha256(line bytes); chain =
// sha256(prev_hash_hex + "\n" + event_hash_hex), seeded from the all-zero
// genesis so an honest daemon and this verifier land on the same root.
export function recomputeRoot(lines) {
  let prev = ZERO_CHAIN_HASH;
  for (const line of lines) {
    const eventHash = sha256hex(Buffer.from(line, "utf8"));
    prev = sha256hex(Buffer.from(`${prev}\n${eventHash}`, "utf8"));
  }
  return prev;
}

// W011 refutation scan: a signed skill action that causally follows an
// untrusted on-chain read, with no skill_context_injected reset for the
// same issuer in between, is refutable. Returns the verdict plus the list
// of refuting signed events pointing back to the untrusted read that set
// them up.
export function scanRefutations(events) {
  const pending = new Map();
  const refutations = [];
  for (const e of events) {
    const issuer = e.issuer?.pubkey;
    const t = e.kind?.type;
    if (t === "skill_context_injected") pending.delete(issuer);
    else if (t === "untrusted_input_observed") pending.set(issuer, e.id);
    else if (t === "skill_tx_signed" && pending.has(issuer)) {
      refutations.push({ signed_event: e.id, after_untrusted: pending.get(issuer) });
    }
  }
  const verdict = refutations.length ? "refute" : "pass";
  return { verdict, refutations };
}
