import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { recomputeAuditRoot } from "../audit/auditRoot";
import type { Witness } from "./types";

// Anchor 2 — local hash chain. attestations/<sha>.json holds per-LLM-call Step
// records Merkle-rooted into audit_root_hex; green when present with a root.
// recomputeAuditRoot (lib/audit/auditRoot.ts) replays the raw event lines the
// same way the daemon and standalone verifier do, so this is a real independent
// check of the published root, not a trust: a chain whose recomputed root does
// not match the published one reads red, as does an attestation missing its root
// or canonical steps, or one that is unreadable.
export function checkAnchor2AuditChain(repoRoot: string, sha: string): Witness {
  const attestationPath = join(repoRoot, "attestations", `${sha}.json`);
  if (!existsSync(attestationPath)) {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "yellow",
      detail:
        "No audit chain published for this commit yet. A run's hash-chained events recompute into attestations/<sha>.json; once present this light recomputes the root and checks it.",
    };
  }
  try {
    const att = JSON.parse(readFileSync(attestationPath, "utf8")) as {
      audit_root_hex?: string;
      steps?: unknown;
    };
    const steps = Array.isArray(att.steps) ? att.steps : [];
    if (!att.audit_root_hex || !steps.length || !steps.every((s) => typeof s === "string")) {
      return {
        key: "audit_chain",
        label: "Audit hash chain",
        state: "red",
        detail: "Attestation present but missing a root or its canonical steps.",
      };
    }
    const recomputed = recomputeAuditRoot(steps as string[]);
    if (recomputed !== att.audit_root_hex) {
      return {
        key: "audit_chain",
        label: "Audit hash chain",
        state: "red",
        detail: `Chain tampered: recomputed root ${recomputed.slice(0, 12)}… does not match the published ${att.audit_root_hex.slice(0, 12)}….`,
      };
    }
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "green",
      detail: `Root ${att.audit_root_hex.slice(0, 16)}… recomputed from ${steps.length} hash-chained events and matches.`,
      drillHref: `https://github.com/open-covenant/covenant/blob/main/attestations/${sha}.json`,
    };
  } catch {
    return {
      key: "audit_chain",
      label: "Audit hash chain",
      state: "red",
      detail: "Attestation unreadable.",
    };
  }
}
