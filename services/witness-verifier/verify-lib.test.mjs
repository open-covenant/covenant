import { describe, it, expect } from "vitest";
import {
  recomputeRoot,
  scanRefutations,
  buildSkillManifest,
  ZERO_CHAIN_HASH,
} from "./verify-lib.mjs";

// The exact roots below were precomputed with an independent hand-rolled
// sha256 fold (not via recomputeRoot) and frozen, so any drift in the
// genesis seed, separator, or concat order breaks the assertion instead of
// silently passing a relational check.
const SINGLE_LINE = '{"id":"e1","kind":{"type":"skill_installed"}}';
const SECOND_LINE =
  '{"id":"e2","issuer":{"pubkey":"pkA"},"kind":{"type":"skill_tx_signed"}}';
const ROOT_EMPTY = "0".repeat(64);
const ROOT_SINGLE = "e023655a9dd8280f7c732d17b0c0d4800b8d4b93e2a627548a2b1d2a0b4e8669";
const ROOT_TWO = "324a74c4f65c148e834d592c7ad8f19ebdcbdacd885b39f6ae4760ef3e4b7ad5";

const ev = (id, pubkey, type) => ({ id, issuer: { pubkey }, kind: { type } });

describe("recomputeRoot", () => {
  it("returns the all-zero genesis for an empty event stream", () => {
    expect(recomputeRoot([])).toBe(ROOT_EMPTY);
    expect(ROOT_EMPTY).toBe(ZERO_CHAIN_HASH);
  });

  it("pins the exact single-event root (genesis seed + first fold)", () => {
    expect(recomputeRoot([SINGLE_LINE])).toBe(ROOT_SINGLE);
  });

  it("pins the exact two-event root (separator + prev->next threading)", () => {
    expect(recomputeRoot([SINGLE_LINE, SECOND_LINE])).toBe(ROOT_TWO);
  });

  it("is order-sensitive: swapping two distinct lines changes the root", () => {
    expect(recomputeRoot([SECOND_LINE, SINGLE_LINE])).not.toBe(ROOT_TWO);
  });
});

describe("scanRefutations", () => {
  it("passes a clean run with no signed-after-untrusted pairing", () => {
    expect(scanRefutations([ev("s1", "pkA", "skill_tx_signed")])).toEqual({
      verdict: "pass",
      refutations: [],
    });
  });

  it("refutes a skill_tx_signed that causally follows an untrusted read", () => {
    const events = [
      ev("u1", "pkA", "untrusted_input_observed"),
      ev("s1", "pkA", "skill_tx_signed"),
    ];
    expect(scanRefutations(events)).toEqual({
      verdict: "refute",
      refutations: [{ signed_event: "s1", after_untrusted: "u1" }],
    });
  });

  it("clears the poison when a skill_context_injected reset lands between them", () => {
    const events = [
      ev("u1", "pkA", "untrusted_input_observed"),
      ev("r1", "pkA", "skill_context_injected"),
      ev("s1", "pkA", "skill_tx_signed"),
    ];
    expect(scanRefutations(events)).toEqual({ verdict: "pass", refutations: [] });
  });

  it("tracks poison per issuer: an untrusted read for A does not refute a signed action by B", () => {
    const events = [
      ev("u1", "pkA", "untrusted_input_observed"),
      ev("s1", "pkB", "skill_tx_signed"),
    ];
    expect(scanRefutations(events)).toEqual({ verdict: "pass", refutations: [] });
  });

  it("poison persists across signed actions until a context reset refutes each one", () => {
    const events = [
      ev("u1", "pkA", "untrusted_input_observed"),
      ev("s1", "pkA", "skill_tx_signed"),
      ev("s2", "pkA", "skill_tx_signed"),
    ];
    expect(scanRefutations(events)).toEqual({
      verdict: "refute",
      refutations: [
        { signed_event: "s1", after_untrusted: "u1" },
        { signed_event: "s2", after_untrusted: "u1" },
      ],
    });
  });

  it("scopes the context reset to its own issuer: B's reset does not clear A's poison", () => {
    const events = [
      ev("u1", "pkA", "untrusted_input_observed"),
      ev("r1", "pkB", "skill_context_injected"),
      ev("s1", "pkA", "skill_tx_signed"),
    ];
    expect(scanRefutations(events)).toEqual({
      verdict: "refute",
      refutations: [{ signed_event: "s1", after_untrusted: "u1" }],
    });
  });
});

describe("buildSkillManifest", () => {
  const installed = (name, digest) => ({
    kind: { type: "skill_installed", name, digest_hex: digest },
  });
  const injected = (name, digest) => ({
    kind: { type: "skill_context_injected", skill_name: name, skill_digest_hex: digest },
  });
  const txSigned = (sig) => ({
    kind: { type: "skill_tx_signed", signature_b58: sig },
  });

  it("defaults to the covenant skill with no digest when no skill events are present", () => {
    expect(buildSkillManifest([])).toEqual({
      skill: { name: "covenant", digest: "" },
      capabilities: ["skill.use.covenant"],
      tx: null,
    });
  });

  it("uses the installed skill when only a skill_installed event is present", () => {
    expect(buildSkillManifest([installed("deploy", "abc")])).toEqual({
      skill: { name: "deploy", digest: "sha256:abc" },
      capabilities: ["skill.use.deploy"],
      tx: null,
    });
  });

  it("prefers the context-injected skill over the installed one", () => {
    const m = buildSkillManifest([installed("deploy", "abc"), injected("audit", "def")]);
    expect(m.skill).toEqual({ name: "audit", digest: "sha256:def" });
    expect(m.capabilities).toEqual(["skill.use.audit"]);
  });

  it("drops the sha256: prefix when the digest is empty", () => {
    expect(buildSkillManifest([injected("audit", "")]).skill.digest).toBe("");
    expect(buildSkillManifest([installed("audit", "")]).skill.digest).toBe("");
  });

  it("emits the tx object only when a skill_tx_signed event is present", () => {
    expect(buildSkillManifest([txSigned("sig42")]).tx).toEqual({
      sig: "sig42",
      cluster: "devnet",
      slot: null,
    });
    expect(buildSkillManifest([]).tx).toBeNull();
  });
});
