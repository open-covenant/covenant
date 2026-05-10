#!/usr/bin/env node
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: package-manager-readiness [--json]

Report package-manager distribution readiness without creating manifests,
tagging releases, signing artifacts, uploading files, or publishing packages.`);
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
for (const arg of args) {
  if (arg !== "--json") {
    usage();
    process.exit(2);
  }
}

const policyDocOk = exists("docs/internal/package-manager-readiness.md");
const distributionBindingOk = exists("agent-os/scripts/distribution-readiness.mjs");
const validatorOk = exists("agent-os/scripts/validate-package-manager-readiness.mjs");

const localEvidence = [
  {
    id: "readiness-document",
    title: "Package-manager readiness document",
    status: policyDocOk ? "documented" : "blocked",
    ok: policyDocOk,
    evidence: policyDocOk ? ["docs/internal/package-manager-readiness.md"] : [],
    blockers: policyDocOk ? [] : ["package-manager readiness document is missing"],
  },
  {
    id: "distribution-readiness-binding",
    title: "Distribution readiness binding",
    status: distributionBindingOk ? "documented" : "blocked",
    ok: distributionBindingOk,
    evidence: distributionBindingOk ? ["agent-os/scripts/distribution-readiness.mjs"] : [],
    blockers: distributionBindingOk ? [] : ["distribution readiness report is missing"],
  },
  {
    id: "validator-contract",
    title: "Package-manager readiness validator",
    status: validatorOk ? "implemented" : "blocked",
    ok: validatorOk,
    evidence: validatorOk ? ["agent-os/scripts/validate-package-manager-readiness.mjs"] : [],
    blockers: validatorOk ? [] : ["package-manager readiness validator is missing"],
  },
];

const requirements = [
  {
    id: "homebrew-manifest",
    title: "Homebrew manifest",
    status: "planned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "formula source URL and checksum are not bound to a release artifact",
      "install, service, caveat, uninstall, and upgrade behavior are not tested",
    ],
    human_decision_required: true,
  },
  {
    id: "nix-manifest",
    title: "Nix package manifest",
    status: "planned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "Nix derivation or flake package is not implemented",
      "hashes, platforms, service module behavior, and binary paths are not validated",
    ],
    human_decision_required: true,
  },
  {
    id: "linux-packages",
    title: "Linux package manifests",
    status: "planned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "Debian and RPM package metadata are not implemented",
      "system service integration, uninstall behavior, and file ownership are not tested",
    ],
    human_decision_required: true,
  },
  {
    id: "artifact-source",
    title: "Artifact source and checksum binding",
    status: "human-owned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "release artifact hosting destination is not approved",
      "package manifests are not bound to release manifest digests",
    ],
    human_decision_required: true,
  },
  {
    id: "signing-verification",
    title: "Release signing verification",
    status: "human-owned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "project release signing key custody is not approved",
      "package-manager install path does not verify published signatures",
    ],
    human_decision_required: true,
  },
  {
    id: "install-uninstall-ci",
    title: "Install and uninstall CI",
    status: "planned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "package install and uninstall checks are not running in CI",
      "upgrade and rollback behavior are not validated across package formats",
    ],
    human_decision_required: false,
  },
  {
    id: "publication-approval",
    title: "Publication approval",
    status: "human-owned",
    ok: false,
    evidence: ["docs/internal/package-manager-readiness.md"],
    blockers: [
      "package registry accounts and publication destinations are not approved",
      "public package announcement and rollback policy are not accepted",
    ],
    human_decision_required: true,
  },
];

const localReady = localEvidence.every((gate) => gate.ok);
const publicReady = requirements.every((requirement) => requirement.ok);
const blockers = [
  ...localEvidence.filter((gate) => !gate.ok).map((gate) => gate.id),
  ...requirements.filter((requirement) => !requirement.ok).map((requirement) => requirement.id),
];

const report = {
  kind: "covenant_package_manager_readiness",
  schema: "covenant.package-manager-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_manifest_review: localReady,
  ready_for_public_packages: publicReady,
  selected_package_channels: [],
  blockers,
  human_decisions: [
    "artifact hosting destination",
    "release signing key custody",
    "package registry accounts",
    "public package publication approval",
    "rollback announcement policy",
  ],
  non_goals: [
    "creating package-manager manifests",
    "tagging releases",
    "signing release artifacts",
    "publishing packages",
  ],
  local_evidence: localEvidence,
  requirements,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`package-manager distribution: ${report.ready_for_public_packages ? "ready" : "blocked"}`);
  console.log(`manifest review evidence: ${report.ready_for_manifest_review ? "ready" : "blocked"}`);
  for (const gate of localEvidence) {
    console.log(`- ${gate.ok ? "ok" : gate.status}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
  for (const requirement of requirements) {
    console.log(`- ${requirement.status}: ${requirement.title}`);
    for (const blocker of requirement.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}
