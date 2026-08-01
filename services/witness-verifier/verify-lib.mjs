// Pure, side-effect-free extraction of verify-run.mjs's audit chain-root fold
// and event-lineage heuristic. Importing this module does no I/O or keygen, so
// it is unit-testable offline.
import { createHash } from "node:crypto";

// All-zero genesis seed, identical to the daemon's covenant-audit
// ZERO_CHAIN_HASH (agent-os/crates/covenant-audit/src/lib.rs:637).
export const ZERO_CHAIN_HASH = "0".repeat(64);
export const WITNESS_SCHEMA = "covenant.witness-verdict.v2";
export const WITNESS_DOMAIN = "covenant.witness-verdict.v2";

const sha256hex = (buf) => createHash("sha256").update(buf).digest("hex");

function canonical(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`)
    .join(",")}}`;
}

export function buildVerifierStatement(
  root,
  eventCount,
  verdict,
  refutations,
  verifierPubkey,
) {
  return {
    schema: WITNESS_SCHEMA,
    domain: WITNESS_DOMAIN,
    audit_root_hex: root,
    event_count: eventCount,
    verdict,
    refutations,
    verifier_pubkey: verifierPubkey,
  };
}

export function verifierMessage(statement) {
  return Buffer.from(`${WITNESS_DOMAIN}\n${canonical(statement)}`, "utf8");
}

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

// Event-lineage heuristic over the supplied log: a signed skill action ordered
// after an untrusted-input event, with no skill_context_injected reset for the
// same issuer in between, is marked refuted. This does not inspect event
// semantics, prove causality or completeness, mediate a runtime, or enforce
// W009/W011. It returns the configured verdict and the matching event ids.
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

// Build the skill-run manifest the /verify page renders. A context-injected
// skill takes precedence over an installed one; the name falls back to
// "covenant" and the digest to "" when neither is present. The digest is
// published with a "sha256:" prefix only when non-empty.
export function buildSkillManifest(events) {
  const installed = events.find((e) => e.kind?.type === "skill_installed");
  const injected = events.find((e) => e.kind?.type === "skill_context_injected");
  const txSigned = events.find((e) => e.kind?.type === "skill_tx_signed");
  const name = injected?.kind.skill_name || installed?.kind.name || "covenant";
  const digestHex = injected?.kind.skill_digest_hex || installed?.kind.digest_hex || "";
  return {
    skill: { name, digest: digestHex ? `sha256:${digestHex}` : "" },
    capabilities: [`skill.use.${name}`],
    tx: txSigned ? { sig: txSigned.kind.signature_b58, cluster: "devnet", slot: null } : null,
  };
}
