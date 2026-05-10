#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/release-provenance-readiness.mjs", "--json"],
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
  console.error(`validate-release-provenance-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_release_provenance_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.release-provenance-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_local_release_provenance_planning !== true) {
  fail("local release provenance planning gates must pass");
}
if (report.ready_for_public_release_provenance !== false) {
  fail("public release provenance must remain blocked until key custody, release subject verification, and transparency publication are implemented");
}

const requirements = new Set(report.artifact_subject_requirements ?? []);
for (const field of [
  "repository",
  "releaseId",
  "commit",
  "artifacts[].name",
  "artifacts[].sha256",
  "artifacts[].sizeBytes",
  "validation evidence",
]) {
  if (!requirements.has(field)) {
    fail(`missing artifact subject requirement: ${field}`);
  }
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "release-subject-schema",
  "local-provenance-verifier",
  "audit-root-signing-policy",
  "review-artifact-signing-contract",
  "project-key-custody",
  "release-artifact-subject-verifier",
  "transparency-publication",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

for (const id of [
  "release-subject-schema",
  "local-provenance-verifier",
  "audit-root-signing-policy",
  "review-artifact-signing-contract",
]) {
  const gate = gates.get(id);
  if (gate && gate.ok !== true) {
    fail(`${id} must pass`);
  }
}

for (const id of [
  "project-key-custody",
  "release-artifact-subject-verifier",
  "transparency-publication",
]) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 4) {
  fail("human provenance decisions must be explicit");
}

if (errors.length > 0) {
  console.error("validate-release-provenance-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-release-provenance-readiness: ok");
