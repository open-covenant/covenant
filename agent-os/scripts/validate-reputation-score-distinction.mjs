#!/usr/bin/env node
// Covenant has three reputation surfaces on three scales: the audit-chain
// compliance score (4-decimal, the only one projected to EVM), the daemon's
// escrow standing (basis points), and the SAP-native peer score the bridge
// proxies (upstream-defined). This guard pairs the reconciliation table in
// docs/multichain-value-capture.md with the identifiers in the source files,
// and refuses the generic names whose ambiguity the split removed.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const read = (rel) => readFileSync(join(repoRoot, rel), "utf8");

const doc = read("docs/multichain-value-capture.md");
const auditRs = read("agent-os/crates/covenant-audit/src/reputation.rs");
const daemonRs = read("agent-os/crates/covenantd/src/reputation.rs");
const discoveryRs = read("agent-os/crates/covenant-sap-bridge/src/discovery.rs");
const identityRs = read("agent-os/crates/covenant-sap-bridge/src/identity.rs");
const workerTs = read("packages/sap-bridge/src/index.ts");

const failures = [];
const need = (haystack, needle, where, why) => {
  if (!haystack.includes(needle)) {
    failures.push(
      `- ${where}: missing \`${needle}\`; remediation: ${why}`,
    );
  }
};
const forbid = (haystack, pattern, where, why) => {
  if (pattern.test(haystack)) {
    failures.push(`- ${where}: matches ${pattern}; remediation: ${why}`);
  }
};

need(
  doc,
  "## Three reputation surfaces",
  "docs/multichain-value-capture.md",
  "restore the reconciliation section",
);
for (const literal of [
  "AuditReputation",
  "EscrowReputation",
  "sap_reputation_score",
  "reputationScore",
  "SCORE_DECIMALS = 4",
  "completion_rate_bps",
]) {
  need(
    doc,
    literal,
    "docs/multichain-value-capture.md",
    "the table must name every surface with its scale",
  );
}
need(
  doc,
  "projected to EVM",
  "docs/multichain-value-capture.md",
  "the table must state which score projects to EVM",
);

need(
  auditRs,
  "pub struct AuditReputation",
  "covenant-audit/src/reputation.rs",
  "keep the compliance score under its distinct type name",
);
need(
  auditRs,
  "pub const SCORE_DECIMALS: u8 = 4",
  "covenant-audit/src/reputation.rs",
  "the doc table pins the 4-decimal scale; update both together",
);

need(
  daemonRs,
  "pub struct EscrowReputation",
  "covenantd/src/reputation.rs",
  "keep the escrow standing under its distinct type name",
);
need(
  daemonRs,
  "completion_rate_bps",
  "covenantd/src/reputation.rs",
  "the rate carries its unit in the field name",
);
forbid(
  daemonRs,
  /pub struct Reputation\b/,
  "covenantd/src/reputation.rs",
  "the bare `Reputation` name is the ambiguity this guard exists to prevent",
);

for (const [rel, src] of [
  ["covenant-sap-bridge/src/discovery.rs", discoveryRs],
  ["covenant-sap-bridge/src/identity.rs", identityRs],
]) {
  need(
    src,
    "pub sap_reputation_score",
    rel,
    "the SAP score stays under its own name on the Rust side",
  );
  need(
    src,
    '#[serde(rename = "reputationScore")]',
    rel,
    "the wire name the worker sends must survive the Rust rename",
  );
  forbid(
    src,
    /pub reputation_score\b/,
    rel,
    "rename to sap_reputation_score with the serde rename pinning the wire",
  );
}

need(
  workerTs,
  "reputationScore",
  "packages/sap-bridge/src/index.ts",
  "the Rust serde rename mirrors this worker field; reconcile both sides",
);

if (failures.length > 0) {
  console.error("validate-reputation-score-distinction: failed");
  for (const f of failures) console.error(f);
  process.exit(1);
}

console.log(
  "validate-reputation-score-distinction: ok (AuditReputation 4-decimal / EscrowReputation bps / sap_reputation_score wire reputationScore)",
);
