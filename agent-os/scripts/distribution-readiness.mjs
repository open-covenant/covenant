#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
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
    id: "package-manager-distribution",
    title: "Package-manager distribution",
    scope: "public_distribution",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "package-manager manifests are not implemented",
      "package install and uninstall paths are not covered in CI",
      "artifact upload destinations are not approved",
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
    status: "planned",
    ok: false,
    evidence: ["packages/sdk/README.md", "packages/sdk-ui/README.md"],
    blockers: [
      "SDK packages are workspace-local and not published",
      "semantic versioning and compatibility policy are not defined",
      "generated protocol bindings are not covered by compatibility fixtures",
    ],
    human_decision_required: true,
  },
  {
    id: "upgrade-policy",
    title: "Upgrade and rollback policy",
    scope: "public_distribution",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "automatic upgrade safety is not implemented",
      "rollback behavior is not documented or tested",
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
  ready_for_source_alpha: sourceInstallOk,
  ready_for_public_distribution: publicGates.every((gate) => gate.ok),
  blockers,
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
