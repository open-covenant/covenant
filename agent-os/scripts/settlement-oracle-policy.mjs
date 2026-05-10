#!/usr/bin/env node
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: settlement-oracle-policy [--json]

Report settlement oracle and pricing policy readiness without selecting a
production oracle, assigning update authority, or writing chain state.`);
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
for (const arg of args) {
  if (arg !== "--json") {
    usage();
    process.exit(2);
  }
}

const policyDocOk = exists("docs/settlement-oracle-policy.md");
const deploymentReadinessOk = exists("agent-os/scripts/settlement-deployment-readiness.mjs");
const validatorOk = exists("agent-os/scripts/validate-settlement-oracle-policy.mjs");

const localEvidence = [
  {
    id: "policy-document",
    title: "Oracle policy document",
    status: policyDocOk ? "documented" : "blocked",
    ok: policyDocOk,
    evidence: policyDocOk ? ["docs/settlement-oracle-policy.md"] : [],
    blockers: policyDocOk ? [] : ["settlement oracle policy document is missing"],
  },
  {
    id: "deployment-readiness-binding",
    title: "Deployment readiness binding",
    status: deploymentReadinessOk ? "documented" : "blocked",
    ok: deploymentReadinessOk,
    evidence: deploymentReadinessOk
      ? ["agent-os/scripts/settlement-deployment-readiness.mjs"]
      : [],
    blockers: deploymentReadinessOk
      ? []
      : ["settlement deployment readiness report is missing"],
  },
  {
    id: "validator-contract",
    title: "Oracle policy validator",
    status: validatorOk ? "implemented" : "blocked",
    ok: validatorOk,
    evidence: validatorOk ? ["agent-os/scripts/validate-settlement-oracle-policy.mjs"] : [],
    blockers: validatorOk ? [] : ["settlement oracle policy validator is missing"],
  },
];

const requirements = [
  {
    id: "source-selection",
    title: "Production oracle source selection",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "primary and fallback production oracle sources are not approved",
      "source ownership, licensing, and dependency provenance are not recorded",
    ],
    human_decision_required: true,
  },
  {
    id: "update-authority",
    title: "Pricing update authority",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "oracle update signer or signer quorum is not assigned",
      "key custody, rotation, and revocation procedures are not approved",
    ],
    human_decision_required: true,
  },
  {
    id: "freshness-and-staleness",
    title: "Freshness and stale-data behavior",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "maximum accepted price age is not bound to a selected source",
      "stale-data rejection and fallback behavior are not tested against source output",
    ],
    human_decision_required: true,
  },
  {
    id: "manipulation-controls",
    title: "Manipulation resistance controls",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "deviation thresholds and circuit-breaker behavior are not approved",
      "selected source manipulation and outage assumptions are not reviewed",
    ],
    human_decision_required: true,
  },
  {
    id: "outage-behavior",
    title: "Oracle outage behavior",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "outage pause, retry, and resume behavior are not approved",
      "operator escalation path for oracle failure is not bound to a release runbook",
    ],
    human_decision_required: true,
  },
  {
    id: "deployment-binding",
    title: "Deployment binding",
    status: "human-owned",
    ok: false,
    evidence: ["docs/settlement-oracle-policy.md"],
    blockers: [
      "oracle account, program configuration, or off-chain publisher binding is not selected",
      "release evidence does not bind the oracle policy to a deployment candidate",
    ],
    human_decision_required: true,
  },
];

const localReady = localEvidence.every((gate) => gate.ok);
const onchainReady = requirements.every((requirement) => requirement.ok);
const blockers = [
  ...localEvidence.filter((gate) => !gate.ok).map((gate) => gate.id),
  ...requirements.filter((requirement) => !requirement.ok).map((requirement) => requirement.id),
];

const report = {
  kind: "covenant_settlement_oracle_policy",
  schema: "covenant.settlement-oracle-policy.v1",
  generated_at: new Date().toISOString(),
  ready_for_policy_review: localReady,
  ready_for_onchain_oracle: onchainReady,
  selected_oracle: null,
  blockers,
  human_decisions: [
    "production oracle source selection",
    "pricing update authority custody",
    "maximum accepted price age",
    "manipulation threshold and circuit-breaker policy",
    "outage pause and resume authority",
    "deployment binding and release evidence acceptance",
  ],
  non_goals: [
    "selecting a production oracle source",
    "assigning custody for oracle update keys",
    "deploying or configuring on-chain oracle accounts",
    "writing chain state",
  ],
  local_evidence: localEvidence,
  requirements,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`settlement oracle policy: ${report.ready_for_onchain_oracle ? "ready" : "blocked"}`);
  console.log(`policy review evidence: ${report.ready_for_policy_review ? "ready" : "blocked"}`);
  for (const gate of localEvidence) {
    console.log(`- ${gate.ok ? "ok" : gate.status}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
  for (const requirement of requirements) {
    console.log(`- ${requirement.status}: ${requirement.title}`);
    for (const blocker of requirement.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}
