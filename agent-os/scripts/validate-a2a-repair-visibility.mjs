#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/a2a-repair-visibility.mjs", "--json"],
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
  console.error(`validate-a2a-repair-visibility: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_a2a_repair_visibility") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.a2a-repair-visibility.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_operator_repair_visibility !== true) {
  fail("operator repair visibility gates must pass");
}
if (report.ready_for_delegated_repair !== false) {
  fail("delegated repair must remain blocked until peer-scoped reporting and denial coverage exist");
}

const requirements = new Set(report.delegated_repair_requirements ?? []);
for (const requirement of [
  "peer-scoped repair report",
  "per-peer skipped retry summary",
  "peer-mismatched repair denial tests",
  "capability-scope denial fixtures",
]) {
  if (!requirements.has(requirement)) {
    fail(`missing delegated repair requirement: ${requirement}`);
  }
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "operator-repair-contract",
  "retry-visibility-contract",
  "cli-repair-surfaces",
  "live-operator-repair-coverage",
  "per-peer-repair-report",
  "delegated-repair-denial-coverage",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

for (const id of [
  "operator-repair-contract",
  "retry-visibility-contract",
  "cli-repair-surfaces",
  "live-operator-repair-coverage",
]) {
  const gate = gates.get(id);
  if (gate && gate.ok !== true) {
    fail(`${id} must pass`);
  }
}

for (const id of ["per-peer-repair-report", "delegated-repair-denial-coverage"]) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
}

if (errors.length > 0) {
  console.error("validate-a2a-repair-visibility: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-a2a-repair-visibility: ok");
