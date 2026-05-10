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
  "live peer-mismatched repair denial coverage",
  "human review before delegated repair automation",
]) {
  if (!requirements.has(requirement)) {
    fail(`missing delegated repair requirement: ${requirement}`);
  }
}

if (report.per_peer_repair_report?.schema !== "covenant.a2a-peer-repair-report.v1") {
  fail("per-peer repair report schema reference missing");
}
if (
  report.per_peer_repair_report?.validator
  !== "node agent-os/scripts/validate-a2a-peer-repair-report.mjs"
) {
  fail("per-peer repair report validator reference missing");
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

const perPeerReport = gates.get("per-peer-repair-report");
if (perPeerReport && perPeerReport.ok !== true) {
  fail("per-peer-repair-report must pass");
}
if (!perPeerReport?.evidence?.includes("agent-os/scripts/a2a-peer-repair-report.mjs")) {
  fail("per-peer-repair-report must name report script evidence");
}

const delegatedDenial = gates.get("delegated-repair-denial-coverage");
if (delegatedDenial) {
  if (delegatedDenial.ok !== false) {
    fail("delegated-repair-denial-coverage must not be reported ready yet");
  }
  if (delegatedDenial.status !== "partial") {
    fail("delegated-repair-denial-coverage must be partial until live coverage lands");
  }
  if (!delegatedDenial.evidence?.includes("docs/a2a-repair-authorization.md")) {
    fail("delegated-repair-denial-coverage must name authorization policy evidence");
  }
  if (!Array.isArray(delegatedDenial.blockers) || delegatedDenial.blockers.length === 0) {
    fail("delegated-repair-denial-coverage must list blockers");
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
