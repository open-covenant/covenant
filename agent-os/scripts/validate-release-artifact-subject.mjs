#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function run(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/release-artifact-subject.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

const fixtureRoot = mkdtempSync(join(tmpdir(), "covenant-release-subject-"));
const artifactRoot = join(fixtureRoot, "artifacts");
mkdirSync(artifactRoot);

const sourcePath = join(artifactRoot, "covenant-source.tar.gz");
const checksumPath = join(artifactRoot, "checksums.txt");
writeFileSync(sourcePath, "source artifact\n");
writeFileSync(checksumPath, `${sha256(sourcePath)}  covenant-source.tar.gz\n`);

const subjectPath = join(fixtureRoot, "release-subject.json");
const subject = {
  schema: "covenant.provenance.release.v1",
  generatedAt: "2026-05-10T00:00:00.000Z",
  subject: {
    kind: "release_bundle",
    repository: "open-covenant/covenant",
    releaseId: "v0.1.0-alpha.1",
    tag: "v0.1.0-alpha.1",
    commit: "0123456789abcdef0123456789abcdef01234567",
    artifacts: [
      {
        name: "covenant-source",
        filename: "covenant-source.tar.gz",
        sha256: sha256(sourcePath),
        sizeBytes: readFileSync(sourcePath).length,
      },
      {
        name: "checksums",
        filename: "checksums.txt",
        sha256: sha256(checksumPath),
        sizeBytes: readFileSync(checksumPath).length,
      },
    ],
  },
};
writeJson(subjectPath, subject);

const valid = run([
  "--subject",
  subjectPath,
  "--artifact-root",
  artifactRoot,
  "--json",
]);
if (valid.status !== 0) {
  process.stderr.write(valid.stderr || valid.stdout);
  process.exit(valid.status ?? 1);
}

let report;
try {
  report = JSON.parse(valid.stdout);
} catch (error) {
  console.error(`validate-release-artifact-subject: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_release_artifact_subject") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.release-artifact-subject.v1") {
  fail("unexpected report schema");
}
if (report.subject_schema !== "covenant.provenance.release.v1") {
  fail("unexpected subject schema");
}
if (report.ok !== true) {
  fail("valid release subject must pass");
}
if (!Array.isArray(report.artifacts) || report.artifacts.length !== 2) {
  fail("valid release subject must report two artifacts");
}
if (report.artifact_root !== "<external>") {
  fail("absolute artifact roots must be redacted in the report");
}
if (!Array.isArray(report.non_goals) || !report.non_goals.includes("publishing artifacts")) {
  fail("report must state that it does not publish artifacts");
}

const badDigestPath = join(fixtureRoot, "bad-digest.json");
const badDigest = structuredClone(subject);
badDigest.subject.artifacts[0].sha256 = "0".repeat(64);
writeJson(badDigestPath, badDigest);
const invalidDigest = run([
  "--subject",
  badDigestPath,
  "--artifact-root",
  artifactRoot,
  "--json",
]);
if (invalidDigest.status === 0) {
  fail("sha256 mismatch must fail");
}
if (!/sha256 does not match/.test(invalidDigest.stdout + invalidDigest.stderr)) {
  fail("sha256 mismatch must be reported");
}

const badPath = join(fixtureRoot, "bad-path.json");
const badPathSubject = structuredClone(subject);
badPathSubject.subject.artifacts[0].filename = "../outside";
writeJson(badPath, badPathSubject);
const invalidPath = run([
  "--subject",
  badPath,
  "--artifact-root",
  artifactRoot,
  "--json",
]);
if (invalidPath.status === 0) {
  fail("path traversal artifact filename must fail");
}
if (!/safe relative path/.test(invalidPath.stdout + invalidPath.stderr)) {
  fail("path traversal failure must be reported");
}

if (errors.length > 0) {
  console.error("validate-release-artifact-subject: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-release-artifact-subject: ok");
