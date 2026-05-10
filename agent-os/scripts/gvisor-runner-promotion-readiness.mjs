#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: gvisor-runner-promotion-readiness [--json] [--strict]

Aggregate the gVisor runner metadata into per-precondition CI promotion signals.
Does not install runsc, build a rootfs, run live tests, change CI configuration,
or accept a runner record. Promotion remains human-owned.

Default mode exits 0 and reports blockers. Use --strict to exit non-zero while
any precondition is blocked.`);
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}
const asJson = args.has("--json");
const strict = args.has("--strict");
for (const arg of args) {
  if (!["--json", "--strict"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const upstream = spawnSync(
  process.execPath,
  ["agent-os/scripts/gvisor-host-readiness.mjs", "--json"],
  {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  },
);
if (upstream.status !== 0) {
  process.stderr.write(upstream.stderr || upstream.stdout);
  process.exit(upstream.status ?? 1);
}

let host;
try {
  host = JSON.parse(upstream.stdout);
} catch (error) {
  console.error(`gvisor-runner-promotion-readiness: upstream JSON parse failed: ${error.message}`);
  process.exit(1);
}

const runner = host.runner_metadata ?? {};
const hostGates = new Map((host.gates ?? []).map((gate) => [gate.id, gate]));

const hostProvisioningOk = ["linux-host", "runsc-runtime", "rootfs-shell"].every(
  (id) => hostGates.get(id)?.ok === true,
);
const hostProvisioningBlockers = ["linux-host", "runsc-runtime", "rootfs-shell"]
  .map((id) => hostGates.get(id))
  .filter((gate) => gate && gate.ok !== true)
  .flatMap((gate) => gate.blockers ?? [`${gate.id} is not ready`]);

const nonEmptyString = (value) => typeof value === "string" && value.length > 0;
const runscImageOk =
  nonEmptyString(runner?.runsc?.source) &&
  nonEmptyString(runner?.runsc?.checksum_url) &&
  nonEmptyString(runner?.runsc?.release_id);
const rootfsDigestOk =
  nonEmptyString(runner?.rootfs?.source) &&
  nonEmptyString(runner?.rootfs?.checksum_url) &&
  nonEmptyString(runner?.rootfs?.release_id);
const hostBaselineOk =
  nonEmptyString(runner?.host?.ci_runner_label) &&
  nonEmptyString(runner?.host?.target_arch) &&
  nonEmptyString(runner?.rootfs?.architecture);
const failurePolicyOk =
  nonEmptyString(runner?.policy?.failure_mode) &&
  nonEmptyString(runner?.policy?.unsupported_host_policy) &&
  nonEmptyString(runner?.policy?.required_scope);
const runnerRecordAccepted = runner?.status === "accepted";

const preconditions = [
  {
    id: "host-provisioning",
    title: "Linux host provisioning",
    ok: hostProvisioningOk,
    evidence: ["agent-os/scripts/gvisor-host-readiness.mjs"],
    blockers: hostProvisioningOk ? [] : hostProvisioningBlockers,
    human_decision_required: true,
  },
  {
    id: "runsc-image-digest",
    title: "Pinned runsc release source",
    ok: runscImageOk,
    evidence: ["agent-os/ci/gvisor-runner-record.json"],
    blockers: runscImageOk
      ? []
      : ["runner_metadata.runsc.source, checksum_url, and release_id must be pinned"],
    human_decision_required: true,
  },
  {
    id: "rootfs-digest",
    title: "Pinned rootfs release source",
    ok: rootfsDigestOk,
    evidence: ["agent-os/ci/gvisor-runner-record.json"],
    blockers: rootfsDigestOk
      ? []
      : ["runner_metadata.rootfs.source, checksum_url, and release_id must be pinned"],
    human_decision_required: true,
  },
  {
    id: "host-architecture-pinned",
    title: "CI runner label and architecture baseline",
    ok: hostBaselineOk,
    evidence: ["agent-os/ci/gvisor-runner-record.json"],
    blockers: hostBaselineOk
      ? []
      : ["runner_metadata.host.ci_runner_label, host.target_arch, and rootfs.architecture must be recorded"],
    human_decision_required: true,
  },
  {
    id: "failure-policy",
    title: "Sandbox failure policy",
    ok: failurePolicyOk,
    evidence: ["docs/runtime-sandbox-security.md", "agent-os/ci/gvisor-runner-record.json"],
    blockers: failurePolicyOk
      ? []
      : [
          "runner_metadata.policy.failure_mode, unsupported_host_policy, and required_scope must be approved",
        ],
    human_decision_required: true,
  },
  {
    id: "runner-record-accepted",
    title: "Accepted runner metadata record",
    ok: runnerRecordAccepted,
    evidence: ["agent-os/ci/gvisor-runner-record.json"],
    blockers: runnerRecordAccepted
      ? []
      : [`runner_metadata.status must be "accepted" (currently "${runner?.status ?? "unknown"}")`],
    human_decision_required: true,
  },
];

const readyForPromotion = preconditions.every((precondition) => precondition.ok);
const blockers = preconditions
  .filter((precondition) => !precondition.ok)
  .map((precondition) => precondition.id);

const report = {
  kind: "covenant_gvisor_runner_promotion_readiness",
  schema: "covenant.gvisor-runner-promotion-readiness.v1",
  generated_at: new Date().toISOString(),
  host_readiness_schema: host.schema ?? null,
  runner_metadata_schema: runner?.schema ?? null,
  runner_metadata_status: runner?.status ?? null,
  ready_for_promotion: readyForPromotion,
  blockers,
  preconditions,
  human_decisions: [
    "CI runner image acceptance",
    "rootfs artifact provenance",
    "host kernel and architecture baseline",
    "sandbox failure policy",
    "required-job scope and rollback policy",
  ],
  non_goals: [
    "installing runsc",
    "building a rootfs",
    "running live tests",
    "changing CI configuration",
    "accepting a runner metadata record",
  ],
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `gvisor runner promotion: ${report.ready_for_promotion ? "ready" : "blocked"}`,
  );
  console.log(`runner metadata status: ${report.runner_metadata_status ?? "unknown"}`);
  for (const precondition of preconditions) {
    const marker = precondition.ok ? "ok" : "blocked";
    console.log(`- ${marker}: ${precondition.title}`);
    for (const blocker of precondition.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strict && !report.ready_for_promotion) {
  process.exit(1);
}
