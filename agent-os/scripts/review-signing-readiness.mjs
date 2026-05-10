#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: review-signing-readiness [--json] [--strict-signed]

Report whether autonomy review artifacts are ready to become signed release
evidence. The report is read-only and never creates keys, signatures, files, or
Git metadata.

Default mode exits 0 and reports blockers. Use --strict-signed in release
automation that would require signed review artifacts.`);
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

function contains(path, text) {
  return exists(path) && read(path).includes(text);
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strictSigned = args.has("--strict-signed");
for (const arg of args) {
  if (!["--json", "--strict-signed"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const reviewToolchainOk = exists("agent-os/scripts/autonomy-review-artifact.mjs")
  && exists("agent-os/scripts/autonomy-verify-review-artifact.mjs")
  && exists("agent-os/scripts/validate-autonomy-review-artifacts.mjs");
const signingContractOk = contains(
  "docs/provenance/review-artifact-signing.md",
  "covenant.autonomy-review-signature.v1",
) && contains(
  "docs/provenance/review-artifact-signing.md",
  "project review signing key custody",
);

const gates = [
  {
    id: "unsigned-review-artifact-toolchain",
    title: "Unsigned review artifact toolchain",
    status: reviewToolchainOk ? "implemented" : "missing",
    ok: reviewToolchainOk,
    evidence: [
      "agent-os/scripts/autonomy-review-artifact.mjs",
      "agent-os/scripts/autonomy-verify-review-artifact.mjs",
      "agent-os/scripts/validate-autonomy-review-artifacts.mjs",
    ],
    blockers: reviewToolchainOk ? [] : ["review artifact generator, verifier, or validator is missing"],
    human_decision_required: false,
  },
  {
    id: "review-signing-contract",
    title: "Review signing contract",
    status: signingContractOk ? "documented" : "missing",
    ok: signingContractOk,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: signingContractOk ? [] : ["review artifact signing contract is missing"],
    human_decision_required: false,
  },
  {
    id: "project-review-key-custody",
    title: "Project review key custody",
    status: "human_required",
    ok: false,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: ["project review signing key custody is not approved"],
    human_decision_required: true,
  },
  {
    id: "public-key-publication",
    title: "Public key publication",
    status: "human_required",
    ok: false,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: ["trusted public key publication location is not approved"],
    human_decision_required: true,
  },
  {
    id: "rotation-revocation-policy",
    title: "Rotation and revocation policy",
    status: "human_required",
    ok: false,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: ["review signing key rotation and revocation policy is not approved"],
    human_decision_required: true,
  },
  {
    id: "release-evidence-policy",
    title: "Release evidence policy",
    status: "human_required",
    ok: false,
    evidence: ["docs/provenance/review-artifact-signing.md"],
    blockers: ["signed review artifacts are not approved as release-grade evidence"],
    human_decision_required: true,
  },
];

const blockers = gates.filter((gate) => !gate.ok).map((gate) => gate.id);

const report = {
  kind: "covenant_review_signing_readiness",
  schema: "covenant.review-signing-readiness.v1",
  generated_at: new Date().toISOString(),
  ready_for_unsigned_review_artifacts: reviewToolchainOk,
  ready_for_signed_review_artifacts: false,
  blockers,
  project_key: {
    status: "not_selected",
    key_id: null,
    public_key_spki_sha256: null,
    public_key_source: null,
    custody_policy: null,
    local_key_material_recorded: false,
  },
  human_decisions: [
    "project review signing key custody",
    "public key publication location",
    "key id, rotation, and revocation policy",
    "authorized review artifact signers",
    "release evidence acceptance policy",
  ],
  non_goals: [
    "creating a project signing key",
    "publishing a trusted public key",
    "signing review artifacts",
    "treating fixture signatures as release evidence",
  ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`review signing readiness: ${report.ready_for_signed_review_artifacts ? "signed-ready" : "blocked"}`);
  console.log(`unsigned review artifacts: ${report.ready_for_unsigned_review_artifacts ? "ready" : "blocked"}`);
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strictSigned && !report.ready_for_signed_review_artifacts) {
  process.exit(1);
}
