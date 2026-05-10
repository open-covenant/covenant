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
  fail("delegated repair must remain blocked until human release review approves automation");
}

const requirements = new Set(report.delegated_repair_requirements ?? []);
if (!requirements.has("human release review marker before delegated repair automation")) {
  fail("missing delegated repair requirement: human release review marker before delegated repair automation");
}
if (requirements.has("live peer-mismatched repair denial coverage")) {
  fail("live peer-mismatched repair denial coverage should no longer be a remaining requirement");
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
if (report.delegated_repair_release_review?.schema !== "covenant.a2a-repair-release-review.v1") {
  fail("delegated repair release-review report schema reference missing");
}
if (
  report.delegated_repair_release_review?.marker_schema
  !== "covenant.a2a-delegated-repair-release-review.v1"
) {
  fail("delegated repair release-review marker schema reference missing");
}
if (
  report.delegated_repair_release_review?.strict_command
  !== "node agent-os/scripts/a2a-repair-release-review.mjs --strict"
) {
  fail("delegated repair release-review strict command reference missing");
}
if (
  report.delegated_repair_release_review?.validator
  !== "node agent-os/scripts/validate-a2a-repair-release-review.mjs"
) {
  fail("delegated repair release-review validator reference missing");
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "operator-repair-contract",
  "retry-visibility-contract",
  "cli-repair-surfaces",
  "live-operator-repair-coverage",
  "per-peer-repair-report",
  "delegated-repair-denial-coverage",
  "delegated-repair-release-review",
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
  if (delegatedDenial.ok !== true) {
    fail("delegated-repair-denial-coverage must pass after live coverage lands");
  }
  if (delegatedDenial.status !== "implemented") {
    fail("delegated-repair-denial-coverage must be implemented after live coverage lands");
  }
  if (!delegatedDenial.evidence?.includes("docs/a2a-repair-authorization.md")) {
    fail("delegated-repair-denial-coverage must name authorization policy evidence");
  }
  if (!delegatedDenial.evidence?.includes("agent-os/crates/covenantd/tests/live_a2a.rs")) {
    fail("delegated-repair-denial-coverage must name live A2A denial test evidence");
  }
  if (!Array.isArray(delegatedDenial.blockers) || delegatedDenial.blockers.length !== 0) {
    fail("delegated-repair-denial-coverage must not list blockers after live coverage lands");
  }
}

const releaseReview = gates.get("delegated-repair-release-review");
if (releaseReview) {
  if (releaseReview.ok !== false || releaseReview.status !== "human_required") {
    fail("delegated-repair-release-review must remain human_required");
  }
  if (releaseReview.human_decision_required !== true) {
    fail("delegated-repair-release-review must require a human decision");
  }
  if (!Array.isArray(releaseReview.blockers) || releaseReview.blockers.length === 0) {
    fail("delegated-repair-release-review must name the human review blocker");
  }
  for (const path of [
    "docs/decisions/0005-a2a-delegated-repair-release-review.md",
    "agent-os/scripts/a2a-repair-release-review.mjs",
    "agent-os/scripts/validate-a2a-repair-release-review.mjs",
  ]) {
    if (!releaseReview.evidence?.includes(path)) {
      fail(`delegated-repair-release-review must name evidence: ${path}`);
    }
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
