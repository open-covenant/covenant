#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/gvisor-host-readiness.mjs", "--json"],
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
  console.error(`validate-gvisor-host-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_gvisor_host_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.gvisor-host-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_required_ci !== false) {
  fail("required CI promotion must remain blocked until runner, rootfs, and failure policy are approved");
}
if (typeof report.ready_for_local_live_gvisor !== "boolean") {
  fail("ready_for_local_live_gvisor must be boolean");
}
if (!report.host || typeof report.host.platform !== "string") {
  fail("host platform metadata is required");
}
if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 3) {
  fail("human CI promotion decisions must be explicit");
}

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "linux-host",
  "runsc-runtime",
  "rootfs-shell",
  "runtime-policy-evidence",
  "ci-runner-provisioning",
  "rootfs-provenance",
  "mandatory-ci-policy",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

const runtimePolicy = gates.get("runtime-policy-evidence");
if (runtimePolicy && runtimePolicy.ok !== true) {
  fail("runtime-policy-evidence must pass from repository files");
}

for (const id of ["ci-runner-provisioning", "rootfs-provenance", "mandatory-ci-policy"]) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (gate.human_decision_required !== true) {
    fail(`${id} must record human-owned CI promotion authority`);
  }
}

if (errors.length > 0) {
  console.error("validate-gvisor-host-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-gvisor-host-readiness: ok");
