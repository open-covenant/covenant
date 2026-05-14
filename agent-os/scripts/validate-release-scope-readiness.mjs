#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function run(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/release-scope-readiness.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

const errors = [];
const fail = (message) => errors.push(message);

const jsonRun = run(["--json"]);
if (jsonRun.status !== 0) {
  process.stderr.write(jsonRun.stderr || jsonRun.stdout);
  process.exit(jsonRun.status ?? 1);
}

let report;
try {
  report = JSON.parse(jsonRun.stdout);
} catch (error) {
  console.error(`validate-release-scope-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

if (report.kind !== "covenant_release_scope_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.release-scope-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_local_release_scope_planning !== true) {
  fail("local release-scope planning gates must pass once schema, publication location, and inspector are present");
}
if (report.ready_for_public_release_scope !== false) {
  fail("public release-scope publication must remain blocked until generator, signing workflow, and audit-root binding land");
}

const expectedGateIds = [
  "release-scope-schema-doc",
  "release-scope-publication-location",
  "release-scope-manifest-inspector",
  "release-scope-manifest-generator",
  "release-scope-signing-workflow",
  "audit-root-release-scope-binding",
];

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of expectedGateIds) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

const okGates = new Set([
  "release-scope-schema-doc",
  "release-scope-publication-location",
  "release-scope-manifest-inspector",
]);
const plannedGates = new Set([
  "release-scope-manifest-generator",
  "release-scope-signing-workflow",
  "audit-root-release-scope-binding",
]);

for (const id of okGates) {
  const gate = gates.get(id);
  if (gate && gate.ok !== true) {
    fail(`${id} must pass against the current repo state`);
  }
}

for (const id of plannedGates) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (gate.status !== "planned") {
    fail(`${id} must report status planned, got ${gate.status}`);
  }
}

const inspectorGate = gates.get("release-scope-manifest-inspector");
if (inspectorGate) {
  for (const path of [
    "agent-os/scripts/release-scope-manifest.mjs",
    "agent-os/scripts/validate-release-scope-manifest.mjs",
  ]) {
    if (!Array.isArray(inspectorGate.evidence) || !inspectorGate.evidence.includes(path)) {
      fail(`release-scope-manifest-inspector must list evidence: ${path}`);
    }
  }
}

const signingGate = gates.get("release-scope-signing-workflow");
if (signingGate && signingGate.human_decision_required !== true) {
  fail("release-scope-signing-workflow must require human decision");
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 3) {
  fail("human decisions must be explicit");
}

if (!Array.isArray(report.non_goals)
  || !report.non_goals.includes("signing release-scope manifests")
  || !report.non_goals.includes("publishing release-scope manifests")) {
  fail("non_goals must declare the script does not sign or publish manifests");
}

const expectedBlockers = new Set([
  "release-scope-manifest-generator",
  "release-scope-signing-workflow",
  "audit-root-release-scope-binding",
]);
if (!Array.isArray(report.blockers)) {
  fail("blockers must be an array");
} else {
  for (const id of expectedBlockers) {
    if (!report.blockers.includes(id)) {
      fail(`blockers must include: ${id}`);
    }
  }
  for (const id of report.blockers) {
    if (okGates.has(id)) {
      fail(`blockers must not include an ok gate: ${id}`);
    }
  }
}

const strictRun = run(["--strict-public"]);
if (strictRun.status === 0) {
  fail("--strict-public must exit non-zero while public release-scope publication is blocked");
}

const helpRun = run(["--help"]);
if (helpRun.status !== 0) {
  fail("--help must exit 0");
}
if (!/usage: release-scope-readiness/.test(helpRun.stdout)) {
  fail("--help must print usage");
}

const unknownFlagRun = run(["--nope"]);
if (unknownFlagRun.status === 0) {
  fail("unknown flag must exit non-zero");
}

const defaultRun = run([]);
if (defaultRun.status !== 0) {
  fail("default invocation without --strict-public must exit 0");
}
if (!/release-scope readiness:/.test(defaultRun.stdout)) {
  fail("default invocation must print human-readable summary");
}

if (errors.length > 0) {
  console.error("validate-release-scope-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-release-scope-readiness: ok");
