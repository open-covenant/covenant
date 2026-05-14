#!/usr/bin/env node
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: release-scope-readiness [--json] [--strict-public]

Report covenant.release-scope.v1 publication readiness without generating
manifests, signing them, uploading them, or writing transparency-log entries.

Default mode exits 0 and reports blockers. Use --strict-public to exit non-zero
while public release-scope publication is blocked.`);
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

function containsAll(path, needles) {
  if (!exists(path)) return false;
  const text = read(path);
  return needles.every((needle) => text.includes(needle));
}

function workflowFiles() {
  const dir = join(repoRoot, ".github", "workflows");
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((file) => file.endsWith(".yml") || file.endsWith(".yaml"))
    .map((file) => `.github/workflows/${file}`);
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

const schemaDocOk = containsAll("docs/provenance/release-scopes.md", [
  "covenant.release-scope.v1",
  "generatedAt",
  "release.repository",
  "release.commit",
  "release.previousTag",
  "scope.task_count",
  "scope.task_state_distribution",
  "scope.task_set_sha256",
  "non_claims",
  "Canonicalization for `task_set_sha256`",
]);

const publicationLocationOk =
  exists("docs/provenance/release-scopes/README.md") &&
  containsAll("docs/provenance/release-scopes/README.md", [
    "covenant.release-scope.v1",
    "cosign verify-blob",
  ]);

const manifestInspectorOk =
  exists("agent-os/scripts/release-scope-manifest.mjs") &&
  exists("agent-os/scripts/validate-release-scope-manifest.mjs") &&
  containsAll("docs/provenance/release-scopes.md", [
    "release-scope-manifest.mjs",
    "validate-release-scope-manifest.mjs",
  ]);

const manifestGeneratorOk =
  exists("agent-os/scripts/build-release-scope-manifest.mjs") ||
  exists("agent-os/scripts/release-scope-manifest-build.mjs");

const signingWorkflowOk = workflowFiles().some((path) => {
  const text = read(path);
  if (text.includes("docs/provenance/release-scopes/")) return true;
  return text.includes("release-scope") && text.includes("cosign");
});

const auditRootScopeBindingOk =
  containsAll("agent-os/scripts/provenance.mjs", ["releaseScopeSha256"]);

const gates = [
  {
    id: "release-scope-schema-doc",
    title: "Release-scope manifest schema documented",
    status: schemaDocOk ? "documented" : "missing",
    ok: schemaDocOk,
    evidence: ["docs/provenance/release-scopes.md"],
    blockers: schemaDocOk
      ? []
      : ["release-scope schema doc does not pin required fields or canonicalization"],
    human_decision_required: false,
  },
  {
    id: "release-scope-publication-location",
    title: "Release-scope publication location",
    status: publicationLocationOk ? "documented" : "missing",
    ok: publicationLocationOk,
    evidence: ["docs/provenance/release-scopes/README.md"],
    blockers: publicationLocationOk
      ? []
      : ["release-scope publication location is missing or does not describe verification"],
    human_decision_required: false,
  },
  {
    id: "release-scope-manifest-inspector",
    title: "Release-scope manifest inspector",
    status: manifestInspectorOk ? "implemented" : "missing",
    ok: manifestInspectorOk,
    evidence: [
      "agent-os/scripts/release-scope-manifest.mjs",
      "agent-os/scripts/validate-release-scope-manifest.mjs",
      "docs/provenance/release-scopes.md",
    ],
    blockers: manifestInspectorOk
      ? []
      : [
          "release-scope manifest inspector or validator is missing",
          "release-scopes doc does not reference the inspector",
        ],
    human_decision_required: false,
  },
  {
    id: "release-scope-manifest-generator",
    title: "Release-scope manifest generator",
    status: manifestGeneratorOk ? "implemented" : "planned",
    ok: manifestGeneratorOk,
    evidence: [
      "agent-os/scripts/build-release-scope-manifest.mjs",
      "docs/provenance/release-scopes.md",
    ],
    blockers: manifestGeneratorOk
      ? []
      : [
          "release-scope manifest generator does not exist",
          "task corpus selection rules are documented but no script implements them",
          "no validator pins canonicalization reproducibility for the digest",
        ],
    human_decision_required: false,
  },
  {
    id: "release-scope-signing-workflow",
    title: "Release-scope signing workflow",
    status: signingWorkflowOk ? "implemented" : "planned",
    ok: signingWorkflowOk,
    evidence: [".github/workflows/sign-release-artifacts.yml"],
    blockers: signingWorkflowOk
      ? []
      : [
          "no GitHub Actions workflow signs <tag>.json release-scope manifests via cosign keyless",
          "signing identity pins for the release-scope artifact are not yet wired into CI",
        ],
    human_decision_required: true,
  },
  {
    id: "audit-root-release-scope-binding",
    title: "Audit-root binding to release-scope digest",
    status: auditRootScopeBindingOk ? "implemented" : "planned",
    ok: auditRootScopeBindingOk,
    evidence: ["agent-os/scripts/provenance.mjs"],
    blockers: auditRootScopeBindingOk
      ? []
      : [
          "provenance.mjs does not yet bind audit-root attestations to releaseScopeSha256",
          "audit-root self-test does not exercise release-scope binding",
        ],
    human_decision_required: false,
  },
];

const localGateIds = new Set([
  "release-scope-schema-doc",
  "release-scope-publication-location",
  "release-scope-manifest-inspector",
]);
const localGates = gates.filter((gate) => localGateIds.has(gate.id));
const blockers = gates.filter((gate) => !gate.ok).map((gate) => gate.id);

const report = {
  kind: "covenant_release_scope_readiness",
  schema: "covenant.release-scope-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_local_release_scope_planning: localGates.every((gate) => gate.ok),
  ready_for_public_release_scope: gates.every((gate) => gate.ok),
  blockers,
  human_decisions: [
    "release-scope signing workflow extension authority",
    "release-scope manifest commit timing (pre-tag vs tag-time)",
    "release-evidence floor acceptance for release-scope manifests",
  ],
  non_goals: [
    "generating release-scope manifests",
    "signing release-scope manifests",
    "publishing release-scope manifests",
    "writing transparency-log entries",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `release-scope readiness: ${
      report.ready_for_public_release_scope ? "public-ready" : "blocked"
    }`,
  );
  console.log(
    `local planning: ${
      report.ready_for_local_release_scope_planning ? "ready" : "blocked"
    }`,
  );
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strictPublic && !report.ready_for_public_release_scope) {
  process.exit(1);
}
