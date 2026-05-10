#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: distribution-readiness [--json] [--strict-public]

Report distribution and SDK graduation readiness without tagging, signing, publishing,
or uploading artifacts.

Default mode exits 0 and reports blockers. Use --strict-public to exit non-zero
while public distribution gates are blocked.`);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trim(),
    stderr: (result.stderr || "").trim(),
  };
}

function textOutput(result) {
  return [result.stdout, result.stderr].filter(Boolean).join("\n");
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strictPublic = args.has("--strict-public");
for (const arg of args) {
  if (!["--json", "--strict-public"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const sourceInstall = run(process.execPath, ["agent-os/scripts/validate-source-installer.mjs"]);
const sourceInstallOk = sourceInstall.status === 0;
const sourceUpgrade = run(process.execPath, ["agent-os/scripts/validate-source-install-upgrade-plan.mjs"]);
const sourceUpgradeOk = sourceUpgrade.status === 0;
const sourceRollback = run(process.execPath, ["agent-os/scripts/validate-source-install-rollback.mjs"]);
const sourceRollbackOk = sourceRollback.status === 0;
const sourceAlphaOk = sourceInstallOk && sourceUpgradeOk && sourceRollbackOk;
const sdkCompatibility = run(process.execPath, ["agent-os/scripts/validate-sdk-compatibility.mjs"]);
const sdkCompatibilityOk = sdkCompatibility.status === 0;
const packageReadinessDocOk = exists("docs/package-manager-readiness.md");
const packageReadinessReportOk = exists("agent-os/scripts/package-manager-readiness.mjs");
const packageReadinessValidatorOk = exists("agent-os/scripts/validate-package-manager-readiness.mjs");
const packageReadinessEvidenceOk =
  packageReadinessDocOk && packageReadinessReportOk && packageReadinessValidatorOk;

const packageManagerManifestContract = {
  schema: "covenant.package-manager-manifest.v1",
  status: "draft_empty_placeholders",
  ready_for_manifest_review: false,
  manifest_path: null,
  required_fields: [
    "channel",
    "package_name",
    "manifest_path",
    "artifact_url",
    "artifact_sha256",
    "signature_verification",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
  ],
  redaction: {
    machine_local_paths_allowed: false,
    generated_from_local_state: false,
  },
  channels: [
    "homebrew",
    "nix",
    "debian",
    "rpm",
  ].map((channel) => ({
    channel,
    status: "placeholder",
    package_name: null,
    manifest_path: null,
    artifact_url: null,
    artifact_sha256: null,
    signature_verification: null,
    install_check: null,
    uninstall_check: null,
    upgrade_check: null,
    rollback_check: null,
    local_paths_allowed: false,
  })),
};

const gates = [
  {
    id: "source-alpha-install",
    title: "Source alpha install path",
    scope: "source_alpha",
    status: sourceInstallOk ? "implemented" : "blocked",
    ok: sourceInstallOk,
    command: "node agent-os/scripts/validate-source-installer.mjs",
    evidence: [
      "agent-os/scripts/install-source.mjs",
      "agent-os/scripts/validate-source-installer.mjs",
      "docs/source-install.md",
    ],
    blockers: sourceInstallOk ? [] : ["source installer dry-run validation failed"],
    human_decision_required: false,
    output: textOutput(sourceInstall),
  },
  {
    id: "source-upgrade-preflight",
    title: "Source install upgrade preflight",
    scope: "source_alpha",
    status: sourceUpgradeOk ? "implemented" : "blocked",
    ok: sourceUpgradeOk,
    command: "node agent-os/scripts/validate-source-install-upgrade-plan.mjs",
    evidence: [
      "agent-os/scripts/source-install-upgrade-plan.mjs",
      "agent-os/scripts/validate-source-install-upgrade-plan.mjs",
      "docs/source-install.md",
    ],
    blockers: sourceUpgradeOk ? [] : ["source install upgrade preflight validation failed"],
    human_decision_required: false,
    output: textOutput(sourceUpgrade),
  },
  {
    id: "source-rollback-checkpoint",
    title: "Source install rollback checkpoint",
    scope: "source_alpha",
    status: sourceRollbackOk ? "implemented" : "blocked",
    ok: sourceRollbackOk,
    command: "node agent-os/scripts/validate-source-install-rollback.mjs",
    evidence: [
      "agent-os/scripts/install-source.mjs",
      "agent-os/scripts/source-install-rollback.mjs",
      "agent-os/scripts/validate-source-install-rollback.mjs",
      "docs/source-install.md",
    ],
    blockers: sourceRollbackOk ? [] : ["source install rollback checkpoint validation failed"],
    human_decision_required: false,
    output: textOutput(sourceRollback),
  },
  {
    id: "sdk-compatibility-policy",
    title: "SDK compatibility policy",
    scope: "sdk_distribution",
    status: sdkCompatibilityOk ? "implemented" : "blocked",
    ok: sdkCompatibilityOk,
    command: "node agent-os/scripts/validate-sdk-compatibility.mjs",
    evidence: [
      "docs/sdk-compatibility.md",
      "agent-os/scripts/sdk-compatibility.mjs",
      "agent-os/scripts/validate-sdk-compatibility.mjs",
      "packages/sdk/README.md",
      "packages/sdk-ui/README.md",
    ],
    blockers: sdkCompatibilityOk ? [] : ["SDK compatibility validation failed"],
    human_decision_required: false,
    output: textOutput(sdkCompatibility),
  },
  {
    id: "package-manager-distribution",
    title: "Package-manager distribution",
    scope: "public_distribution",
    status: packageReadinessEvidenceOk ? "documented" : "planned",
    ok: false,
    evidence: [
      ...(packageReadinessDocOk ? ["docs/package-manager-readiness.md"] : []),
      ...(packageReadinessReportOk ? ["agent-os/scripts/package-manager-readiness.mjs"] : []),
      ...(packageReadinessValidatorOk
        ? ["agent-os/scripts/validate-package-manager-readiness.mjs"]
        : []),
      "agent-os/scripts/distribution-readiness.mjs",
      "agent-os/scripts/validate-distribution-readiness.mjs",
      "docs/distribution-readiness.md",
    ],
    blockers: [
      "package-manager manifests are not implemented for Homebrew, Nix, Debian, or RPM",
      "package-manager manifest contract contains draft placeholders only",
      "package install, uninstall, upgrade, and rollback paths are not covered in CI",
      "artifact hosting, signing, checksum, and publication destinations are not approved",
    ],
    human_decision_required: true,
  },
  {
    id: "signed-release-artifacts",
    title: "Signed release artifacts",
    scope: "public_distribution",
    status: "planned",
    ok: false,
    evidence: ["docs/provenance/release-subjects.md"],
    blockers: [
      "project signing key custody is not selected",
      "release artifact subjects are not verified in CI",
      "signature publication and revocation policy is not approved",
    ],
    human_decision_required: true,
  },
  {
    id: "sdk-stability",
    title: "SDK stability boundary",
    scope: "sdk_distribution",
    status: sdkCompatibilityOk ? "experimental" : "blocked",
    ok: false,
    evidence: [
      "docs/sdk-compatibility.md",
      "agent-os/scripts/sdk-compatibility.mjs",
      "agent-os/scripts/validate-sdk-compatibility.mjs",
      "packages/sdk/README.md",
      "packages/sdk-ui/README.md",
    ],
    blockers: [
      "SDK packages are workspace-local and not published",
      "public semantic versioning support window is not approved",
      "generated protocol bindings are not covered by compatibility fixtures",
    ],
    human_decision_required: true,
  },
  {
    id: "upgrade-policy",
    title: "Upgrade and rollback policy",
    scope: "public_distribution",
    status: sourceUpgradeOk ? "experimental" : "blocked",
    ok: false,
    evidence: [
      "agent-os/scripts/source-install-upgrade-plan.mjs",
      "agent-os/scripts/source-install-rollback.mjs",
      "agent-os/scripts/validate-source-install-upgrade-plan.mjs",
      "agent-os/scripts/validate-source-install-rollback.mjs",
      "docs/source-install.md",
    ],
    blockers: [
      "source rollback is local-prefix only and not package-manager rollback",
      "public rollback policy is not approved",
      "installer migration checks are not covered across releases",
    ],
    human_decision_required: true,
  },
];

const publicGateIds = new Set([
  "package-manager-distribution",
  "signed-release-artifacts",
  "sdk-stability",
  "upgrade-policy",
]);

const publicGates = gates.filter((gate) => publicGateIds.has(gate.id));
const blockers = gates
  .filter((gate) => !gate.ok)
  .map((gate) => gate.id);

const report = {
  kind: "covenant_distribution_readiness",
  schema: "covenant.distribution-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_source_alpha: sourceAlphaOk,
  ready_for_public_distribution: publicGates.every((gate) => gate.ok),
  blockers,
  package_manager_manifest: packageManagerManifestContract,
  human_decisions: [
    "artifact upload destinations",
    "project signing key custody",
    "SDK stability commitment",
    "public package publication",
    "upgrade announcement language",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `distribution readiness: ${
      report.ready_for_public_distribution ? "public distribution ready" : "public distribution blocked"
    }`,
  );
  console.log(`source alpha install: ${report.ready_for_source_alpha ? "ready" : "blocked"}`);
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strictPublic && !report.ready_for_public_distribution) {
  process.exit(1);
}
