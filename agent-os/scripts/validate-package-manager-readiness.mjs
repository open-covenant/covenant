#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(process.execPath, [
  "agent-os/scripts/package-manager-readiness.mjs",
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
  console.error(`validate-package-manager-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_package_manager_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.package-manager-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_manifest_review !== true) {
  fail("package-manager readiness must be ready for local manifest review");
}
if (report.ready_for_public_packages !== false) {
  fail("public package readiness must remain blocked until manifests, signing, CI, and publication pass");
}
if (!Array.isArray(report.selected_package_channels) || report.selected_package_channels.length !== 0) {
  fail("local readiness report must not select publication channels");
}

const localEvidence = new Map((report.local_evidence ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "readiness-document",
  "distribution-readiness-binding",
  "validator-contract",
]) {
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
  "homebrew-manifest",
  "nix-manifest",
  "linux-packages",
  "artifact-source",
  "signing-verification",
  "install-uninstall-ci",
  "publication-approval",
];
const requirements = new Map((report.requirements ?? []).map((gate) => [gate.id, gate]));
for (const id of requirementIds) {
  const requirement = requirements.get(id);
  if (!requirement) {
    fail(`missing package-manager requirement: ${id}`);
    continue;
  }
  if (requirement.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(requirement.blockers) || requirement.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (!Array.isArray(report.blockers) || !report.blockers.includes(id)) {
    fail(`${id} must be listed as a top-level blocker`);
  }
}

for (const id of [
  "artifact-source",
  "signing-verification",
  "publication-approval",
]) {
  const requirement = requirements.get(id);
  if (requirement && requirement.human_decision_required !== true) {
    fail(`${id} must record human-owned publication authority`);
  }
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 5) {
  fail("package-manager human decisions must be explicit");
}
if (!Array.isArray(report.non_goals) || !report.non_goals.includes("publishing packages")) {
  fail("report must state that it does not publish packages");
}

if (errors.length > 0) {
  console.error("validate-package-manager-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-package-manager-readiness: ok");
