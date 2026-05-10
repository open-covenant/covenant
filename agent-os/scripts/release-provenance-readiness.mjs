#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: release-provenance-readiness [--json] [--strict-public]

Report release provenance readiness without generating keys, signing releases,
publishing artifacts, or writing transparency-log entries.

Default mode exits 0 and reports blockers. Use --strict-public to exit non-zero
while public release provenance gates are blocked.`);
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

function containsAll(path, needles) {
  if (!exists(path)) return false;
  const text = read(path);
  return needles.every((needle) => text.includes(needle));
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

const releaseSubjectsOk = containsAll("docs/provenance/release-subjects.md", [
  "release_bundle",
  "repository",
  "releaseId",
  "commit",
  "artifacts",
  "sha256",
  "sizeBytes",
]);
const provenanceVerifierOk = exists("agent-os/scripts/provenance.mjs")
  && containsAll("docs/provenance/README.md", [
    "provenance envelopes",
    "verify-all",
  ]);
const auditRootPolicyOk = containsAll("docs/decisions/0004-audit-root-signing-policy.md", [
  "project-controlled signing key",
  "GitHub Actions OIDC",
  "offline project key",
]);
const auditRootReleaseSubjectOk = containsAll("agent-os/scripts/provenance.mjs", [
  "releaseSubjectSha256",
  "release subject metadata mismatch",
])
  && containsAll("agent-os/scripts/provenance-self-test.mjs", [
    "release-subject.json",
    "release subject metadata mismatch",
  ]);
const reviewSigningOk = containsAll("docs/provenance/review-artifact-signing.md", [
  "covenant.autonomy-review-signature.v1",
  "human_approval_required",
]);
const releaseArtifactSubjectVerifierOk =
  exists("agent-os/scripts/release-artifact-subject.mjs") &&
  exists("agent-os/scripts/validate-release-artifact-subject.mjs") &&
  containsAll("docs/provenance/release-subjects.md", [
    "release-artifact-subject.mjs",
    "validate-release-artifact-subject.mjs",
  ]);

const gates = [
  {
    id: "release-subject-schema",
    title: "Release artifact subject schema",
    status: releaseSubjectsOk ? "documented" : "missing",
    ok: releaseSubjectsOk,
    evidence: ["docs/provenance/release-subjects.md"],
    blockers: releaseSubjectsOk ? [] : ["release subject schema does not define required artifact metadata"],
    human_decision_required: false,
  },
  {
    id: "local-provenance-verifier",
    title: "Local provenance verifier",
    status: provenanceVerifierOk ? "implemented" : "missing",
    ok: provenanceVerifierOk,
    evidence: ["agent-os/scripts/provenance.mjs", "docs/provenance/README.md"],
    blockers: provenanceVerifierOk ? [] : ["local provenance verifier or docs are missing"],
    human_decision_required: false,
  },
  {
    id: "audit-root-signing-policy",
    title: "Audit-root signing policy",
    status: auditRootPolicyOk ? "documented" : "missing",
    ok: auditRootPolicyOk,
    evidence: ["docs/decisions/0004-audit-root-signing-policy.md"],
    blockers: auditRootPolicyOk ? [] : ["audit-root signing policy does not define acceptable project signing identities"],
    human_decision_required: true,
  },
  {
    id: "audit-root-release-subject-binding",
    title: "Audit-root release subject binding",
    status: auditRootReleaseSubjectOk ? "implemented" : "missing",
    ok: auditRootReleaseSubjectOk,
    evidence: ["agent-os/scripts/provenance.mjs", "agent-os/scripts/provenance-self-test.mjs"],
    blockers: auditRootReleaseSubjectOk ? [] : ["audit-root verifier does not bind release targets to release subject digests"],
    human_decision_required: false,
  },
  {
    id: "review-artifact-signing-contract",
    title: "Review artifact signing contract",
    status: reviewSigningOk ? "documented" : "missing",
    ok: reviewSigningOk,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: reviewSigningOk ? [] : ["review artifact signing contract is missing"],
    human_decision_required: true,
  },
  {
    id: "project-key-custody",
    title: "Project key custody",
    status: "planned",
    ok: false,
    evidence: ["docs/decisions/0004-audit-root-signing-policy.md"],
    blockers: [
      "project release signing key is not selected",
      "public key publication location is not approved",
      "rotation and revocation process is not approved",
    ],
    human_decision_required: true,
  },
  {
    id: "release-artifact-subject-verifier",
    title: "Release artifact subject verifier",
    status: releaseArtifactSubjectVerifierOk ? "implemented" : "planned",
    ok: releaseArtifactSubjectVerifierOk,
    evidence: [
      "docs/provenance/release-subjects.md",
      "agent-os/scripts/release-artifact-subject.mjs",
      "agent-os/scripts/validate-release-artifact-subject.mjs",
    ],
    blockers: releaseArtifactSubjectVerifierOk
      ? []
      : [
          "release_bundle subjects are documented but not implemented in a local verifier",
          "artifact digests are not checked against produced release files",
          "validation evidence is not bound to release artifact subjects",
        ],
    human_decision_required: false,
  },
  {
    id: "transparency-publication",
    title: "Transparency publication",
    status: "planned",
    ok: false,
    evidence: [],
    blockers: [
      "transparency publication target is not selected",
      "publication authentication and retry policy are not approved",
      "published entry verification is not implemented",
    ],
    human_decision_required: true,
  },
];

const localGateIds = new Set([
  "release-subject-schema",
  "local-provenance-verifier",
  "audit-root-signing-policy",
  "audit-root-release-subject-binding",
  "review-artifact-signing-contract",
  "release-artifact-subject-verifier",
]);
const localGates = gates.filter((gate) => localGateIds.has(gate.id));
const blockers = gates.filter((gate) => !gate.ok).map((gate) => gate.id);

const report = {
  kind: "covenant_release_provenance_readiness",
  schema: "covenant.release-provenance-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_local_release_provenance_planning: localGates.every((gate) => gate.ok),
  ready_for_public_release_provenance: false,
  artifact_subject_requirements: [
    "repository",
    "releaseId",
    "commit",
    "artifacts[].name",
    "artifacts[].sha256",
    "artifacts[].sizeBytes",
    "validation evidence",
  ],
  blockers,
  human_decisions: [
    "project release signing key custody",
    "public key publication location",
    "key rotation and revocation process",
    "transparency publication target",
    "release evidence acceptance policy",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(
    `release provenance readiness: ${
      report.ready_for_public_release_provenance ? "public-ready" : "blocked"
    }`,
  );
  console.log(
    `local planning: ${
      report.ready_for_local_release_provenance_planning ? "ready" : "blocked"
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

if (strictPublic && !report.ready_for_public_release_provenance) {
  process.exit(1);
}
