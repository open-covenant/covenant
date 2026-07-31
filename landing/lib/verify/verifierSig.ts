import { createPublicKey, verify as edVerify } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Witness } from "./types";

const SCHEMA = "covenant.witness-verdict.v2";
const DOMAIN = "covenant.witness-verdict.v2";

type Refutation = { signed_event: string; after_untrusted: string };
type VerifierStatement = {
  schema: typeof SCHEMA;
  domain: typeof DOMAIN;
  audit_root_hex: string;
  event_count: number;
  verdict: "pass" | "refute";
  refutations: Refutation[];
  verifier_pubkey: string;
};

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function exactKeys(
  value: Record<string, unknown>,
  expected: string[],
): boolean {
  return (
    Object.keys(value).sort().join("\0") === [...expected].sort().join("\0")
  );
}

function parseStatement(value: unknown): VerifierStatement | null {
  const raw = object(value);
  if (
    !raw ||
    !exactKeys(raw, [
      "schema",
      "domain",
      "audit_root_hex",
      "event_count",
      "verdict",
      "refutations",
      "verifier_pubkey",
    ]) ||
    raw.schema !== SCHEMA ||
    raw.domain !== DOMAIN ||
    typeof raw.audit_root_hex !== "string" ||
    !/^[0-9a-f]{64}$/.test(raw.audit_root_hex) ||
    !Number.isSafeInteger(raw.event_count) ||
    (raw.event_count as number) < 0 ||
    (raw.verdict !== "pass" && raw.verdict !== "refute") ||
    typeof raw.verifier_pubkey !== "string" ||
    !Array.isArray(raw.refutations)
  ) {
    return null;
  }
  const refutations: Refutation[] = [];
  for (const value of raw.refutations) {
    const item = object(value);
    if (
      !item ||
      !exactKeys(item, ["signed_event", "after_untrusted"]) ||
      typeof item.signed_event !== "string" ||
      typeof item.after_untrusted !== "string"
    ) {
      return null;
    }
    refutations.push({
      signed_event: item.signed_event,
      after_untrusted: item.after_untrusted,
    });
  }
  if ((raw.verdict === "pass") !== (refutations.length === 0)) return null;
  return {
    schema: SCHEMA,
    domain: DOMAIN,
    audit_root_hex: raw.audit_root_hex,
    event_count: raw.event_count as number,
    verdict: raw.verdict,
    refutations,
    verifier_pubkey: raw.verifier_pubkey,
  };
}

function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.entries(value as Record<string, unknown>)
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`)
    .join(",")}}`;
}

// Anchor 4 validates a closed, signed statement that binds the root, verdict,
// refutations, event count, and signing key. Every accepted v2 artifact must
// also publish a commit-scoped copy of that key. Both live in the same mutable
// repository, so a valid signature is self-consistency, not an external trust
// root, publisher identity, or independent verdict. The global latest-key
// pointer is deliberately not a historical key.
export function checkAnchor4VerifierSig(
  repoRoot: string,
  sha: string,
): Witness {
  const sigPath = join(repoRoot, "attestations", `${sha}.verifier.sig`);
  if (!existsSync(sigPath)) {
    return {
      key: "verifier_sig",
      label: "Self-published verifier statement",
      state: "yellow",
      detail: "No v2 verifier statement is published for this commit.",
    };
  }
  try {
    const att = JSON.parse(
      readFileSync(join(repoRoot, "attestations", `${sha}.json`), "utf8"),
    ) as Record<string, unknown>;
    const statement = parseStatement(att.verifier_statement);
    const steps = Array.isArray(att.steps) ? att.steps : null;
    if (
      !statement ||
      att.audit_root_hex !== statement.audit_root_hex ||
      att.event_count !== statement.event_count ||
      !steps ||
      steps.length !== statement.event_count
    ) {
      return {
        key: "verifier_sig",
        label: "Self-published verifier statement",
        state: "red",
        detail:
          "Verifier artifact is legacy or malformed, or its signed statement does not bind the published root and event count.",
      };
    }
    const versionedPubkeyPath = join(
      repoRoot,
      "landing",
      "public",
      "witness",
      "verifier-keys",
      `${sha}.txt`,
    );
    if (!existsSync(versionedPubkeyPath)) {
      return {
        key: "verifier_sig",
        label: "Self-published verifier statement",
        state: "red",
        detail:
          "The v2 statement has no commit-scoped self-published key. Its embedded key cannot authenticate its own trust root.",
      };
    }
    const pubkey = readFileSync(versionedPubkeyPath, "utf8").trim();
    const sig = readFileSync(sigPath, "utf8").trim();
    if (statement.verifier_pubkey !== pubkey) {
      return {
        key: "verifier_sig",
        label: "Self-published verifier statement",
        state: "red",
        detail:
          "The signed statement does not match its commit-scoped self-published key.",
      };
    }
    const message = Buffer.from(`${DOMAIN}\n${canonical(statement)}`, "utf8");
    const key = createPublicKey({
      format: "jwk",
      key: { kty: "OKP", crv: "Ed25519", x: pubkey },
    });
    if (!edVerify(null, message, key, Buffer.from(sig, "base64url"))) {
      return {
        key: "verifier_sig",
        label: "Self-published verifier statement",
        state: "red",
        detail:
          "The closed v2 statement did not verify against its self-published key.",
      };
    }
    if (statement.verdict === "refute") {
      return {
        key: "verifier_sig",
        label: "Self-published verifier statement",
        state: "red",
        detail: `The signed v2 statement reports ${statement.refutations.length} configured event-order refutation(s) under self-published key ${pubkey.slice(0, 12)}….`,
      };
    }
    return {
      key: "verifier_sig",
      label: "Self-published verifier statement",
      state: "yellow",
      detail: `The closed v2 statement is internally consistent under self-published key ${pubkey.slice(0, 12)}…. Because the key ships beside the artifact, this proves key possession and byte consistency. It is not an externally pinned trust root, publisher identity, semantic validation, completeness proof, runtime mediation, or W009/W011 enforcement.`,
    };
  } catch {
    return {
      key: "verifier_sig",
      label: "Self-published verifier statement",
      state: "red",
      detail: "Verifier signature or pubkey unreadable.",
    };
  }
}
