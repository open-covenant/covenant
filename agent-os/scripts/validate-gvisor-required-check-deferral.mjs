#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// gVisor required-check promotion is deferred to 1.0: the runner record stays
// accepted, and the live workflow stays advisory (path-scoped, manually
// dispatchable, gated on an accepted record) rather than a blanket required
// gate. This guard pins that posture against the two committed artifacts that
// carry it — agent-os/ci/gvisor-runner-record.json and
// .github/workflows/gvisor-live.yml — so a change that silently drops the
// record off "accepted", unpins a runsc/rootfs/host/policy field, deletes the
// workflow, or widens it past the sandbox-runtime path scope fails the gate.
//
// It reads only committed files, so it runs the same on a fresh checkout as it
// does in a full deployment. The promotion-readiness reporter and the decision
// record live on the internal surface and are validated there.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const RECORD_PATH = "agent-os/ci/gvisor-runner-record.json";
const WORKFLOW_PATH = ".github/workflows/gvisor-live.yml";
const RUNTIME_CRATE_GLOB = "agent-os/crates/covenant-runtime/**";

const isString = (value) => typeof value === "string" && value.length > 0;

function evaluate({ record, workflow }) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (!record || typeof record !== "object") {
    fail(`${RECORD_PATH} is missing or not an object`);
  } else {
    if (record.status !== "accepted") {
      fail(`runner record status must stay "accepted" (got ${record.status ?? "unknown"})`);
    }
    if (!(isString(record.runsc?.release_id) && isString(record.runsc?.release_url) && isString(record.runsc?.checksum_url))) {
      fail("runsc release_id, release_url, and checksum_url must stay pinned");
    }
    if (!(isString(record.rootfs?.release_id) && isString(record.rootfs?.release_url) && isString(record.rootfs?.checksum_url))) {
      fail("rootfs release_id, release_url, and checksum_url must stay pinned");
    }
    if (!(isString(record.host?.ci_runner_label) && isString(record.host?.arch) && isString(record.rootfs?.architecture))) {
      fail("host.ci_runner_label, host.arch, and rootfs.architecture must stay recorded");
    }
    if (!(isString(record.policy?.failure_mode) && isString(record.policy?.unsupported_host_policy) && isString(record.policy?.required_scope))) {
      fail("policy.failure_mode, unsupported_host_policy, and required_scope must stay approved");
    }
  }

  if (workflow == null) {
    fail(`${WORKFLOW_PATH} must stay present`);
  } else {
    if (!/\bworkflow_dispatch:/.test(workflow)) {
      fail("gvisor-live.yml must keep workflow_dispatch so a runsc-capable contributor can run it on demand");
    }
    if (!/\bpull_request:/.test(workflow)) {
      fail("gvisor-live.yml must keep a pull_request trigger");
    }
    if (!workflow.includes("paths:")) {
      fail("gvisor-live.yml must stay path-scoped (advisory), not a blanket required gate");
    }
    if (!workflow.includes(RUNTIME_CRATE_GLOB)) {
      fail(`gvisor-live.yml path scope must include ${RUNTIME_CRATE_GLOB}`);
    }
    if (!workflow.includes('!= "accepted"')) {
      fail("gvisor-live.yml must keep the accepted-record gate before running live CI");
    }
  }

  return errors;
}

function goodInputs() {
  return {
    record: {
      status: "accepted",
      runsc: { release_id: "20240701.0", release_url: "https://example/runsc", checksum_url: "https://example/runsc.sha512" },
      rootfs: { release_id: "3.20.3", release_url: "https://example/rootfs.tar.gz", checksum_url: "https://example/rootfs.tar.gz.sha256", architecture: "x86_64" },
      host: { ci_runner_label: "ubuntu-22.04", arch: "x86_64" },
      policy: { failure_mode: "fail-closed", unsupported_host_policy: "skip-with-evidence", required_scope: "sandbox-runtime-changes-only" },
    },
    workflow: [
      "on:",
      "  pull_request:",
      "    paths:",
      `      - '${RUNTIME_CRATE_GLOB}'`,
      "  workflow_dispatch:",
      'if [ "$status" != "accepted" ]; then',
    ].join("\n"),
  };
}

function runSelfTest() {
  const failures = [];

  if (evaluate(goodInputs()).length > 0) {
    failures.push(`good fixture should pass but reported: ${evaluate(goodInputs()).join("; ")}`);
  }

  const badCases = [
    ["record dropped accepted", (i) => (i.record.status = "candidate")],
    ["record missing entirely", (i) => (i.record = null)],
    ["runsc checksum unpinned", (i) => delete i.record.runsc.checksum_url],
    ["rootfs release_url unpinned", (i) => delete i.record.rootfs.release_url],
    ["rootfs architecture dropped", (i) => delete i.record.rootfs.architecture],
    ["host runner label dropped", (i) => delete i.record.host.ci_runner_label],
    ["host arch dropped", (i) => delete i.record.host.arch],
    ["failure policy weakened away", (i) => delete i.record.policy.required_scope],
    ["workflow deleted", (i) => (i.workflow = null)],
    ["workflow lost workflow_dispatch", (i) => (i.workflow = i.workflow.replace("  workflow_dispatch:", ""))],
    ["workflow lost pull_request", (i) => (i.workflow = i.workflow.replace("  pull_request:", ""))],
    ["workflow lost path scope", (i) => (i.workflow = i.workflow.replace("    paths:", ""))],
    ["workflow widened past runtime scope", (i) => (i.workflow = i.workflow.replace(RUNTIME_CRATE_GLOB, "**"))],
    ["workflow lost accepted gate", (i) => (i.workflow = i.workflow.replace('!= "accepted"', "== yes"))],
  ];

  for (const [label, mutate] of badCases) {
    const input = goodInputs();
    mutate(input);
    if (evaluate(input).length === 0) {
      failures.push(`bad fixture "${label}" should have been rejected but passed`);
    }
  }

  return failures;
}

function readText(relativePath) {
  try {
    return readFileSync(join(repoRoot, relativePath), "utf8");
  } catch {
    return null;
  }
}

function liveInputs() {
  let record = null;
  const recordText = readText(RECORD_PATH);
  if (recordText != null) {
    try {
      record = JSON.parse(recordText);
    } catch (error) {
      console.error(`validate-gvisor-required-check-deferral: ${RECORD_PATH} is not JSON: ${error.message}`);
      process.exit(1);
    }
  }
  return { record, workflow: readText(WORKFLOW_PATH) };
}

const args = new Set(process.argv.slice(2));
for (const arg of args) {
  if (!["--self-test", "--help", "-h"].includes(arg)) {
    console.error("usage: validate-gvisor-required-check-deferral [--self-test]");
    process.exit(2);
  }
}
if (args.has("--help") || args.has("-h")) {
  console.log("usage: validate-gvisor-required-check-deferral [--self-test]\n\nPins the gVisor required-check deferral posture: accepted runner record + advisory live workflow.");
  process.exit(0);
}

const selfTestFailures = runSelfTest();
if (selfTestFailures.length > 0) {
  console.error("validate-gvisor-required-check-deferral: self-test failed");
  for (const failure of selfTestFailures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}
if (args.has("--self-test")) {
  console.log("validate-gvisor-required-check-deferral: self-test ok");
  process.exit(0);
}

const errors = evaluate(liveInputs());
if (errors.length > 0) {
  console.error("validate-gvisor-required-check-deferral: deferral posture drifted");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-gvisor-required-check-deferral: ok");
