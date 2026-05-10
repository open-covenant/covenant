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

const runnerMetadata = report.runner_metadata;
if (!runnerMetadata || runnerMetadata.schema !== "covenant.gvisor-runner-metadata.v1") {
  fail("runner metadata schema must be covenant.gvisor-runner-metadata.v1");
}
if (runnerMetadata?.status !== "unpinned") {
  fail("runner metadata must remain unpinned until CI promotion evidence is accepted");
}
if (runnerMetadata?.ready_for_required_ci !== false) {
  fail("runner metadata must not report required CI readiness yet");
}
if (runnerMetadata?.redaction?.local_paths_recorded !== false) {
  fail("runner metadata must not record local paths");
}

const requiredFields = new Set(runnerMetadata?.required_fields ?? []);
for (const field of [
  "schema",
  "runsc.version",
  "runsc.source",
  "runsc.digest_sha256",
  "rootfs.source",
  "rootfs.digest_sha256",
  "rootfs.architecture",
  "host.platform",
  "host.arch",
  "host.kernel",
  "policy.failure_mode",
]) {
  if (!requiredFields.has(field)) {
    fail(`runner metadata required field missing: ${field}`);
  }
}
if (runnerMetadata?.runsc?.digest_sha256 !== null) {
  fail("runsc digest must remain null until a pinned runner is accepted");
}
if (runnerMetadata?.rootfs?.digest_sha256 !== null) {
  fail("rootfs digest must remain null until a pinned rootfs is accepted");
}
if (runnerMetadata?.host?.platform !== report.host.platform) {
  fail("runner metadata host platform must match report host platform");
}
if (runnerMetadata?.host?.arch !== report.host.arch) {
  fail("runner metadata host architecture must match report host architecture");
}

function visit(value, path = "runner_metadata") {
  if (typeof value === "string") {
    if (value.startsWith("/") || value.includes("\\") || value.includes("$HOME")) {
      fail(`${path} must not contain local paths`);
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => visit(item, `${path}[${index}]`));
    return;
  }
  if (value && typeof value === "object") {
    for (const [key, nested] of Object.entries(value)) {
      visit(nested, `${path}.${key}`);
    }
  }
}
visit(runnerMetadata);

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "linux-host",
  "runsc-runtime",
  "rootfs-shell",
  "runtime-policy-evidence",
  "runner-metadata-schema",
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

const runnerMetadataSchema = gates.get("runner-metadata-schema");
if (runnerMetadataSchema) {
  if (runnerMetadataSchema.ok !== true) {
    fail("runner-metadata-schema must pass from repository files");
  }
  for (const path of [
    "docs/gvisor-host-readiness.md",
    "agent-os/scripts/gvisor-host-readiness.mjs",
    "agent-os/scripts/validate-gvisor-host-readiness.mjs",
  ]) {
    if (!runnerMetadataSchema.evidence?.includes(path)) {
      fail(`runner-metadata-schema must name evidence: ${path}`);
    }
  }
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
