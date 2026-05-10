#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/settlement-deployment-readiness.mjs", "--json"],
  {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  },
);

if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

let report;
try {
  report = JSON.parse(result.stdout);
} catch (error) {
  console.error(`validate-settlement-deployment-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_settlement_deployment_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.settlement-deployment-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_local_scaffold !== true) {
  fail("local settlement scaffold must be ready");
}
if (report.ready_for_onchain_deployment !== false) {
  fail("on-chain deployment must remain blocked until review, oracle, mint, and emergency gates pass");
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "program-scaffold",
  "local-receipt-ledger",
  "deployment-runbook",
  "security-review",
  "oracle-policy",
  "mint-authority-policy",
  "emergency-operations",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

for (const id of ["program-scaffold", "local-receipt-ledger", "deployment-runbook"]) {
  const gate = gates.get(id);
  if (gate && gate.ok !== true) {
    fail(`${id} must pass`);
  }
}

for (const id of [
  "security-review",
  "oracle-policy",
  "mint-authority-policy",
  "emergency-operations",
]) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (gate.human_decision_required !== true) {
    fail(`${id} must record human-owned authority`);
  }
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 5) {
  fail("human deployment decisions must be explicit");
}

if (errors.length > 0) {
  console.error("validate-settlement-deployment-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-settlement-deployment-readiness: ok");
