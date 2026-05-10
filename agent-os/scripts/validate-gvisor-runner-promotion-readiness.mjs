#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/gvisor-runner-promotion-readiness.mjs", "--json"],
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
  console.error(`validate-gvisor-runner-promotion-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_gvisor_runner_promotion_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.gvisor-runner-promotion-readiness.v1") {
  fail("unexpected report schema");
}
if (report.host_readiness_schema !== "covenant.gvisor-host-readiness.v1") {
  fail("host readiness schema must be covenant.gvisor-host-readiness.v1");
}
if (report.runner_metadata_schema !== "covenant.gvisor-runner-metadata.v1") {
  fail("runner metadata schema must be covenant.gvisor-runner-metadata.v1");
}
if (report.runner_metadata_status !== "unpinned") {
  fail("runner metadata status must remain unpinned until a record is accepted");
}
if (report.ready_for_promotion !== false) {
  fail("promotion must remain blocked until preconditions are satisfied");
}

const expectedIds = [
  "host-provisioning",
  "runsc-image-digest",
  "rootfs-digest",
  "host-architecture-pinned",
  "failure-policy",
  "runner-record-accepted",
];
const preconditions = new Map(
  (report.preconditions ?? []).map((precondition) => [precondition.id, precondition]),
);
for (const id of expectedIds) {
  const precondition = preconditions.get(id);
  if (!precondition) {
    fail(`missing precondition: ${id}`);
    continue;
  }
  if (precondition.human_decision_required !== true) {
    fail(`${id} must record human-owned promotion authority`);
  }
  if (!Array.isArray(precondition.blockers)) {
    fail(`${id} must list blockers as an array`);
  }
  if (id !== "host-provisioning") {
    if (precondition.ok !== false) {
      fail(`${id} must remain blocked while runner metadata is unpinned`);
    }
    if (!Array.isArray(precondition.blockers) || precondition.blockers.length === 0) {
      fail(`${id} must list at least one blocker`);
    }
  }
}

if (!Array.isArray(report.blockers)) {
  fail("blockers must be an array");
} else {
  for (const id of expectedIds) {
    const precondition = preconditions.get(id);
    if (precondition && precondition.ok === false && !report.blockers.includes(id)) {
      fail(`top-level blockers must include: ${id}`);
    }
  }
}

if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 4) {
  fail("human decisions must be explicit");
}
if (!Array.isArray(report.non_goals) || !report.non_goals.includes("changing CI configuration")) {
  fail("report must state that it does not change CI configuration");
}

function visit(value, path = "report") {
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
visit(report);

if (errors.length > 0) {
  console.error("validate-gvisor-runner-promotion-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-gvisor-runner-promotion-readiness: ok");
