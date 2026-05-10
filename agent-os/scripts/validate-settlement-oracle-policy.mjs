#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(process.execPath, [
  "agent-os/scripts/settlement-oracle-policy.mjs",
  "--json",
], {
  cwd: repoRoot,
  encoding: "utf8",
  stdio: ["ignore", "pipe", "pipe"],
});

if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

let report;
try {
  report = JSON.parse(result.stdout);
} catch (error) {
  console.error(`validate-settlement-oracle-policy: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_settlement_oracle_policy") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.settlement-oracle-policy.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_policy_review !== true) {
  fail("oracle policy must be ready for local policy review");
}
if (report.ready_for_onchain_oracle !== false) {
  fail("on-chain oracle readiness must remain blocked until human-owned decisions are recorded");
}
if (report.selected_oracle !== null) {
  fail("local readiness report must not select a production oracle");
}

const localEvidence = new Map((report.local_evidence ?? []).map((gate) => [gate.id, gate]));
for (const id of ["policy-document", "deployment-readiness-binding", "validator-contract"]) {
  const gate = localEvidence.get(id);
  if (!gate) {
    fail(`missing local evidence gate: ${id}`);
    continue;
  }
  if (gate.ok !== true) {
    fail(`${id} must pass`);
  }
  if (!Array.isArray(gate.evidence) || gate.evidence.length === 0) {
    fail(`${id} must list evidence`);
  }
}

const requirementIds = [
  "source-selection",
  "update-authority",
  "freshness-and-staleness",
  "manipulation-controls",
  "outage-behavior",
  "deployment-binding",
];
const requirements = new Map((report.requirements ?? []).map((gate) => [gate.id, gate]));
for (const id of requirementIds) {
  const requirement = requirements.get(id);
  if (!requirement) {
    fail(`missing oracle requirement: ${id}`);
    continue;
  }
  if (requirement.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (requirement.human_decision_required !== true) {
    fail(`${id} must record human-owned authority`);
  }
  if (!Array.isArray(requirement.blockers) || requirement.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (!Array.isArray(report.blockers) || !report.blockers.includes(id)) {
    fail(`${id} must be listed as a top-level blocker`);
  }
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 6) {
  fail("oracle human decisions must be explicit");
}
if (!Array.isArray(report.non_goals) || !report.non_goals.includes("writing chain state")) {
  fail("report must state that it does not write chain state");
}

if (errors.length > 0) {
  console.error("validate-settlement-oracle-policy: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-settlement-oracle-policy: ok");
