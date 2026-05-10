#!/usr/bin/env node
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: settlement-deployment-readiness [--json] [--strict]

Report on-chain settlement deployment readiness without deploying programs,
changing mint authorities, selecting oracles, or writing chain state.

Default mode exits 0 and reports blockers. Use --strict to exit non-zero while
on-chain deployment gates are blocked.`);
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strict = args.has("--strict");
for (const arg of args) {
  if (!["--json", "--strict"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const programScaffoldOk =
  exists("agent-os/programs/settlement/Cargo.toml") &&
  exists("agent-os/programs/settlement/src/lib.rs");
const localReceiptsOk =
  exists("agent-os/crates/covenant-settlement/Cargo.toml") &&
  exists("agent-os/crates/covenant-settlement/src/lib.rs");
const readinessDocOk = exists("docs/on-chain-settlement-readiness.md");

const gates = [
  {
    id: "program-scaffold",
    title: "Settlement program scaffold",
    status: programScaffoldOk ? "implemented" : "blocked",
    ok: programScaffoldOk,
    evidence: [
      "agent-os/programs/settlement/Cargo.toml",
      "agent-os/programs/settlement/src/lib.rs",
    ],
    blockers: programScaffoldOk ? [] : ["settlement program scaffold files are missing"],
    human_decision_required: false,
  },
  {
    id: "local-receipt-ledger",
    title: "Local receipt ledger",
    status: localReceiptsOk ? "implemented" : "blocked",
    ok: localReceiptsOk,
    evidence: [
      "agent-os/crates/covenant-settlement/Cargo.toml",
      "agent-os/crates/covenant-settlement/src/lib.rs",
    ],
    blockers: localReceiptsOk ? [] : ["local settlement receipt crate is missing"],
    human_decision_required: false,
  },
  {
    id: "deployment-runbook",
    title: "Deployment runbook",
    status: readinessDocOk ? "documented" : "blocked",
    ok: readinessDocOk,
    evidence: readinessDocOk ? ["docs/on-chain-settlement-readiness.md"] : [],
    blockers: readinessDocOk ? [] : ["deployment readiness runbook is missing"],
    human_decision_required: false,
  },
  {
    id: "security-review",
    title: "Independent security review",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "no accepted security review is recorded",
      "review scope, findings, and remediation evidence are not bound to a release candidate",
    ],
    human_decision_required: true,
  },
  {
    id: "oracle-policy",
    title: "Oracle and pricing policy",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "oracle sources are not selected",
      "pricing update authority and stale-data behavior are not approved",
      "manipulation and outage handling are not tested",
    ],
    human_decision_required: true,
  },
  {
    id: "mint-authority-policy",
    title: "Mint authority and treasury policy",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "external mint authority custody is not approved",
      "treasury account ownership and rotation are not recorded",
      "authority transfer and freeze conditions are not tested",
    ],
    human_decision_required: true,
  },
  {
    id: "emergency-operations",
    title: "Emergency pause and rollback operations",
    status: "planned",
    ok: false,
    evidence: ["agent-os/programs/settlement/src/lib.rs"],
    blockers: [
      "pause authority runbook is not approved",
      "rollback and redeploy sequencing are not tested",
      "incident communication and signer quorum are not defined",
    ],
    human_decision_required: true,
  },
];

const blockers = gates
  .filter((gate) => !gate.ok)
  .map((gate) => gate.id);

const report = {
  kind: "covenant_settlement_deployment_readiness",
  schema: "covenant.settlement-deployment-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_local_scaffold: programScaffoldOk && localReceiptsOk && readinessDocOk,
  ready_for_onchain_deployment: blockers.length === 0,
  blockers,
  human_decisions: [
    "program deployment approval",
    "security review acceptance",
    "oracle source selection",
    "mint authority custody",
    "treasury ownership and rotation",
    "emergency pause and rollback authority",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `settlement deployment readiness: ${
      report.ready_for_onchain_deployment ? "ready" : "blocked"
    }`,
  );
  console.log(`local scaffold: ${report.ready_for_local_scaffold ? "ready" : "blocked"}`);
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strict && !report.ready_for_onchain_deployment) {
  process.exit(1);
}
