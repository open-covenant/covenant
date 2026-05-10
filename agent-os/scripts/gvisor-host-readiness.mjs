#!/usr/bin/env node
import { accessSync, constants, existsSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: gvisor-host-readiness [--json] [--strict-live]

Report Linux gVisor host readiness without running live tests, installing runsc,
creating rootfs artifacts, or changing CI configuration.

Default mode exits 0 and reports blockers. Use --strict-live to exit non-zero
when the current host cannot run the opt-in live gVisor test.`);
}

function run(command, args) {
  return spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function commandExists(command) {
  if (isAbsolute(command)) {
    return executable(command);
  }
  return run("sh", ["-c", `command -v "$1" >/dev/null 2>&1`, "sh", command]).status === 0;
}

function executable(path) {
  try {
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function rootfsCheck(path) {
  if (!path) {
    return {
      ok: false,
      blocker: "COVENANT_LIVE_GVISOR_ROOTFS is not set",
    };
  }
  const shell = join(path, "bin", "sh");
  if (!existsSync(shell)) {
    return {
      ok: false,
      blocker: "rootfs does not contain bin/sh",
    };
  }
  if (!executable(shell)) {
    return {
      ok: false,
      blocker: "rootfs bin/sh is not executable",
    };
  }
  return { ok: true };
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strictLive = args.has("--strict-live");
for (const arg of args) {
  if (!["--json", "--strict-live"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const runsc = process.env.COVENANT_LIVE_RUNSC || "runsc";
const rootfs = process.env.COVENANT_LIVE_GVISOR_ROOTFS || "";
const isLinux = process.platform === "linux";
const runscAvailable = commandExists(runsc);
const runscVersion = runscAvailable ? run(runsc, ["--version"]) : null;
const runscVersionLine = runscVersion?.status === 0
  ? runscVersion.stdout.split("\n")[0].trim()
  : null;
const rootfsStatus = rootfsCheck(rootfs);
const runtimeEvidenceOk =
  existsSync(join(repoRoot, "agent-os/crates/covenant-runtime/src/lib.rs")) &&
  existsSync(join(repoRoot, "agent-os/crates/covenant-runtime/tests/live_gvisor.rs")) &&
  existsSync(join(repoRoot, "docs/internal/gvisor-live-runner.md"));
const runnerMetadataSchemaOk =
  existsSync(join(repoRoot, "docs/internal/gvisor-host-readiness.md")) &&
  existsSync(join(repoRoot, "agent-os/scripts/gvisor-host-readiness.mjs")) &&
  existsSync(join(repoRoot, "agent-os/scripts/validate-gvisor-host-readiness.mjs"));

const requiredRunnerMetadataFields = [
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
];

const gates = [
  {
    id: "linux-host",
    title: "Linux host",
    status: isLinux ? "ready" : "unsupported-host",
    ok: isLinux,
    evidence: [process.platform, process.arch],
    blockers: isLinux ? [] : ["live gVisor dispatch requires Linux"],
    human_decision_required: false,
  },
  {
    id: "runsc-runtime",
    title: "runsc runtime executable",
    status: runscAvailable ? "ready" : "missing",
    ok: runscAvailable,
    evidence: runscVersion?.status === 0 ? [runscVersion.stdout.split("\n")[0]] : [],
    blockers: runscAvailable ? [] : ["runsc is not available on PATH or COVENANT_LIVE_RUNSC"],
    human_decision_required: false,
  },
  {
    id: "rootfs-shell",
    title: "Rootfs with /bin/sh",
    status: rootfsStatus.ok ? "ready" : "missing",
    ok: rootfsStatus.ok,
    evidence: rootfsStatus.ok ? ["COVENANT_LIVE_GVISOR_ROOTFS contains bin/sh"] : [],
    blockers: rootfsStatus.ok ? [] : [rootfsStatus.blocker],
    human_decision_required: false,
  },
  {
    id: "runtime-policy-evidence",
    title: "Runtime fail-closed policy evidence",
    status: runtimeEvidenceOk ? "documented" : "missing",
    ok: runtimeEvidenceOk,
    evidence: [
      "agent-os/crates/covenant-runtime/src/lib.rs",
      "agent-os/crates/covenant-runtime/tests/live_gvisor.rs",
      "docs/internal/gvisor-live-runner.md",
    ],
    blockers: runtimeEvidenceOk ? [] : ["runtime source, live test, or runner guide is missing"],
    human_decision_required: false,
  },
  {
    id: "runner-metadata-schema",
    title: "Pinned runner metadata schema",
    status: runnerMetadataSchemaOk ? "documented" : "missing",
    ok: runnerMetadataSchemaOk,
    evidence: [
      "docs/internal/gvisor-host-readiness.md",
      "agent-os/scripts/gvisor-host-readiness.mjs",
      "agent-os/scripts/validate-gvisor-host-readiness.mjs",
    ],
    blockers: runnerMetadataSchemaOk ? [] : ["gVisor runner metadata schema is missing"],
    human_decision_required: false,
  },
  {
    id: "ci-runner-provisioning",
    title: "Pinned Linux CI runner provisioning",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "CI runner image or setup step is not pinned",
      "runsc installation provenance is not captured in CI logs",
      "accepted covenant.gvisor-runner-metadata.v1 runner record is missing",
    ],
    human_decision_required: true,
  },
  {
    id: "rootfs-provenance",
    title: "Pinned rootfs provenance",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "rootfs artifact source is not pinned by digest",
      "rootfs architecture compatibility is not recorded by CI",
      "accepted covenant.gvisor-runner-metadata.v1 rootfs record is missing",
    ],
    human_decision_required: true,
  },
  {
    id: "mandatory-ci-policy",
    title: "Mandatory CI failure policy",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "required-job scope is not approved",
      "fallback behavior for unsupported hosts is not part of CI policy",
    ],
    human_decision_required: true,
  },
];

const localLiveGateIds = new Set([
  "linux-host",
  "runsc-runtime",
  "rootfs-shell",
  "runtime-policy-evidence",
]);
const localLiveGates = gates.filter((gate) => localLiveGateIds.has(gate.id));
const blockers = gates
  .filter((gate) => !gate.ok)
  .map((gate) => gate.id);

const report = {
  kind: "covenant_gvisor_host_readiness",
  schema: "covenant.gvisor-host-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_local_live_gvisor: localLiveGates.every((gate) => gate.ok),
  ready_for_required_ci: false,
  blockers,
  runner_metadata: {
    schema: "covenant.gvisor-runner-metadata.v1",
    status: "unpinned",
    ready_for_required_ci: false,
    required_fields: requiredRunnerMetadataFields,
    redaction: {
      local_paths_recorded: false,
      rootfs_path_recorded: false,
      runsc_path_recorded: false,
    },
    runsc: {
      observed_version: runscVersionLine,
      command_source: runsc === "runsc" ? "PATH" : "COVENANT_LIVE_RUNSC",
      source: null,
      digest_sha256: null,
    },
    rootfs: {
      configured: Boolean(rootfs),
      has_bin_sh: rootfsStatus.ok,
      source: null,
      digest_sha256: null,
      architecture: null,
    },
    host: {
      platform: process.platform,
      arch: process.arch,
      kernel: null,
      linux_required: true,
    },
    policy: {
      failure_mode: null,
      unsupported_host_policy: null,
    },
  },
  host: {
    platform: process.platform,
    arch: process.arch,
    rootfs_configured: Boolean(rootfs),
    runsc_configured: runsc !== "runsc",
  },
  human_decisions: [
    "Linux runner image or setup step",
    "rootfs artifact provenance",
    "required CI job scope",
    "sandbox failure policy for unsupported hosts",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `gVisor host readiness: ${report.ready_for_local_live_gvisor ? "live-ready" : "blocked"}`,
  );
  console.log(`required CI promotion: ${report.ready_for_required_ci ? "ready" : "blocked"}`);
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strictLive && !report.ready_for_local_live_gvisor) {
  process.exit(1);
}
