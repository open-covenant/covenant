#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/distribution-readiness.mjs", "--json"],
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
  console.error(`validate-distribution-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_distribution_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.distribution-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_source_alpha !== true) {
  fail("source alpha install gate must be ready");
}
if (report.ready_for_public_distribution !== false) {
  fail("public distribution must remain blocked until package, signing, SDK, and upgrade gates are implemented");
}
if (!Array.isArray(report.gates)) {
  fail("gates must be an array");
}
if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 4) {
  fail("human decisions must be explicit");
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "source-alpha-install",
  "source-upgrade-preflight",
  "source-rollback-checkpoint",
  "package-manager-distribution",
  "signed-release-artifacts",
  "sdk-stability",
  "upgrade-policy",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

const source = gates.get("source-alpha-install");
if (source && source.ok !== true) {
  fail("source alpha install gate must pass");
}

const sourceUpgrade = gates.get("source-upgrade-preflight");
if (sourceUpgrade && sourceUpgrade.ok !== true) {
  fail("source upgrade preflight gate must pass");
}
if (
  sourceUpgrade &&
  !sourceUpgrade.evidence.some((item) => item.endsWith("source-install-upgrade-plan.mjs"))
) {
  fail("source upgrade preflight must list its report script as evidence");
}

const sourceRollback = gates.get("source-rollback-checkpoint");
if (sourceRollback && sourceRollback.ok !== true) {
  fail("source rollback checkpoint gate must pass");
}
if (
  sourceRollback &&
  !sourceRollback.evidence.some((item) => item.endsWith("source-install-rollback.mjs"))
) {
  fail("source rollback checkpoint must list its rollback script as evidence");
}

for (const id of [
  "package-manager-distribution",
  "signed-release-artifacts",
  "sdk-stability",
  "upgrade-policy",
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
    fail(`${id} must record human-owned publication authority`);
  }
}

const signed = gates.get("signed-release-artifacts");
if (
  signed &&
  !signed.blockers.some((blocker) => /project signing key custody/i.test(blocker))
) {
  fail("signed-release-artifacts must call out project signing key custody");
}

const upgrade = gates.get("upgrade-policy");
if (
  upgrade &&
  !upgrade.blockers.some((blocker) => /package-manager rollback/i.test(blocker))
) {
  fail("upgrade-policy must keep public package rollback blocked");
}

if (errors.length > 0) {
  console.error("validate-distribution-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-distribution-readiness: ok");
