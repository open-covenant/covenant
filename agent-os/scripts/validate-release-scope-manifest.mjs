#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");

function run(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/release-scope-manifest.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

const fixtureRoot = mkdtempSync(join(tmpdir(), "covenant-release-scope-"));

const wellFormed = {
  schema: "covenant.release-scope.v1",
  generatedAt: "2026-05-14T00:00:00.000Z",
  release: {
    repository: "open-covenant/covenant",
    releaseId: "v0.1.0",
    tag: "v0.1.0",
    commit: "0123456789abcdef0123456789abcdef01234567",
    previousTag: "v0.0.9",
  },
  scope: {
    task_count: 3,
    task_state_distribution: {
      integrated: 2,
      blocked: 1,
    },
    task_set_sha256: "a".repeat(64),
  },
  non_claims: [
    "task_set_sha256 binds the canonical concatenation of in-scope task records; it does not expose ids, titles, or transition notes",
    "task_set_sha256 does not claim that every task was reviewed by a human",
    "task_set_sha256 does not claim that every task was security-relevant or release-grade individually",
    "task_count and task_state_distribution are aggregate descriptors only",
  ],
};

const wellFormedPath = join(fixtureRoot, "well-formed.json");
writeJson(wellFormedPath, wellFormed);

const errors = [];
const fail = (message) => errors.push(message);

const validRun = run(["--path", wellFormedPath, "--json"]);
if (validRun.status !== 0) {
  fail(`well-formed manifest must pass; stderr: ${validRun.stderr}`);
}
let report;
try {
  report = JSON.parse(validRun.stdout);
} catch (error) {
  fail(`well-formed manifest output is not JSON: ${error.message}`);
  report = null;
}
if (report) {
  if (report.kind !== "covenant_release_scope_manifest_check") {
    fail("unexpected report kind");
  }
  if (report.schema !== "covenant.release-scope-manifest-check.v1") {
    fail("unexpected report schema");
  }
  if (report.manifest_schema !== "covenant.release-scope.v1") {
    fail("unexpected manifest schema");
  }
  if (report.ok !== true) {
    fail("well-formed manifest must report ok:true");
  }
  if (!report.release || report.release.tag !== "v0.1.0") {
    fail("well-formed manifest must echo the release tag");
  }
  if (!report.scope || report.scope.task_count !== 3 || report.scope.distribution_sum !== 3) {
    fail("well-formed manifest must echo task_count and distribution_sum");
  }
  if (report.manifest_path !== "<external>") {
    fail("absolute fixture paths must be redacted as <external>");
  }
  if (!Array.isArray(report.non_goals) || !report.non_goals.includes("publishing manifests")) {
    fail("report must declare it does not publish manifests");
  }
  if (!report.non_goals.includes("recomputing task_set_sha256")) {
    fail("report must declare it does not recompute task_set_sha256");
  }
}

const tamper = (label, mutate, expectedFragment) => {
  const path = join(fixtureRoot, `${label}.json`);
  const value = structuredClone(wellFormed);
  mutate(value);
  writeJson(path, value);
  const result = run(["--path", path, "--json"]);
  const combined = `${result.stdout}\n${result.stderr}`;
  if (result.status === 0) {
    fail(`${label}: tampered manifest must fail`);
    return;
  }
  if (!combined.includes(expectedFragment)) {
    fail(`${label}: expected error fragment "${expectedFragment}" not found; output was: ${combined.trim()}`);
  }
};

tamper(
  "bad-schema",
  (value) => {
    value.schema = "covenant.release-scope.v2";
  },
  "schema must be covenant.release-scope.v1",
);

tamper(
  "bad-generated-at",
  (value) => {
    value.generatedAt = "2026-05-14 00:00:00";
  },
  "generatedAt must be an ISO-8601 UTC timestamp",
);

tamper(
  "bad-commit",
  (value) => {
    value.release.commit = "not-a-sha";
  },
  "release.commit must be a 40-character lowercase hex SHA",
);

tamper(
  "bad-digest",
  (value) => {
    value.scope.task_set_sha256 = "deadbeef";
  },
  "scope.task_set_sha256 must be lowercase 64-character hex",
);

tamper(
  "distribution-sum-mismatch",
  (value) => {
    value.scope.task_state_distribution = { integrated: 1, blocked: 1 };
  },
  "does not equal task_state_distribution sum",
);

tamper(
  "missing-non-claim",
  (value) => {
    value.non_claims = value.non_claims.slice(1);
  },
  "non_claims is missing required entry",
);

tamper(
  "unknown-top-level-field",
  (value) => {
    value.extra = "nope";
  },
  "manifest contains unknown top-level field: extra",
);

tamper(
  "unknown-distribution-state",
  (value) => {
    value.scope.task_state_distribution = { integrated: 2, mystery: 1 };
  },
  "unknown workflow state: mystery",
);

tamper(
  "invalid-repository",
  (value) => {
    value.release.repository = "not a slug";
  },
  "release.repository must be owner/name",
);

tamper(
  "previous-tag-empty-string",
  (value) => {
    value.release.previousTag = "";
  },
  "release.previousTag must be null or a non-empty git tag",
);

const firstReleaseFixture = structuredClone(wellFormed);
firstReleaseFixture.release.previousTag = null;
const firstReleasePath = join(fixtureRoot, "first-release.json");
writeJson(firstReleasePath, firstReleaseFixture);
const firstReleaseRun = run(["--path", firstReleasePath, "--json"]);
if (firstReleaseRun.status !== 0) {
  fail(`first-release fixture (previousTag=null) must pass; stderr: ${firstReleaseRun.stderr}`);
}

const missingPathRun = run(["--path", join(fixtureRoot, "does-not-exist.json"), "--json"]);
if (missingPathRun.status === 0) {
  fail("missing manifest file must fail");
} else if (!/manifest file is missing/.test(missingPathRun.stdout + missingPathRun.stderr)) {
  fail("missing manifest must surface an explicit error");
}

const usageRun = run([]);
if (usageRun.status === 0) {
  fail("missing --path must exit non-zero");
}

const unknownFlagRun = run(["--path", wellFormedPath, "--nope"]);
if (unknownFlagRun.status === 0) {
  fail("unknown flag must exit non-zero");
}

if (errors.length > 0) {
  console.error("validate-release-scope-manifest: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-release-scope-manifest: ok");
