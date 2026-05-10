#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const reportSchema = "covenant.release-artifact-subject.v1";
const subjectSchema = "covenant.provenance.release.v1";
const repositoryPattern = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const releaseIdPattern = /^[A-Za-z0-9_.:@/-]+$/;
const hex40Pattern = /^[0-9a-f]{40}$/;
const hex64Pattern = /^[0-9a-f]{64}$/;
const artifactNamePattern = /^[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,127}$/;
const forbiddenPatterns = [
  /\/Users\//,
  /\/home\/[^/\s"]+/,
  /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/,
  /github[-_]?open[-_]?covenant/i,
];

function usage() {
  console.log(`usage: release-artifact-subject --subject path --artifact-root dir [--json]

Verify a release_bundle subject against local artifact bytes without signing,
publishing, uploading, or writing transparency-log entries.`);
}

function failUsage() {
  usage();
  process.exit(2);
}

function parseArgs(argv) {
  const args = { subject: "", artifactRoot: "", json: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    if (arg === "--json") {
      args.json = true;
      continue;
    }
    if (arg === "--subject") {
      args.subject = argv[index + 1] ?? "";
      index += 1;
      continue;
    }
    if (arg === "--artifact-root") {
      args.artifactRoot = argv[index + 1] ?? "";
      index += 1;
      continue;
    }
    failUsage();
  }
  if (!args.subject || !args.artifactRoot) {
    failUsage();
  }
  return args;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function readJson(path, errors) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    errors.push(`subject: cannot parse JSON: ${error.message}`);
    return null;
  }
}

function displayPath(input, absolute) {
  if (absolute.startsWith(`${repoRoot}${sep}`)) {
    return relative(repoRoot, absolute) || ".";
  }
  return input.startsWith("/") ? "<external>" : input;
}

function assertNoPrivateStrings(value, label, errors) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  for (const pattern of forbiddenPatterns) {
    if (pattern.test(text)) {
      errors.push(`${label} contains a forbidden local identity pattern`);
    }
  }
}

function resolveArtifact(root, filename, errors) {
  if (typeof filename !== "string" || filename.trim() === "") {
    errors.push("artifact filename must be present");
    return null;
  }
  if (filename.startsWith("/") || filename.includes("..") || filename.includes("\\")) {
    errors.push(`artifact filename must be a safe relative path: ${filename}`);
    return null;
  }
  const path = resolve(root, filename);
  if (!path.startsWith(`${root}${sep}`)) {
    errors.push(`artifact filename resolves outside artifact root: ${filename}`);
    return null;
  }
  if (!existsSync(path)) {
    errors.push(`artifact file is missing: ${filename}`);
    return null;
  }
  return path;
}

function verifyArtifact(root, artifact, index, errors) {
  const label = `artifacts[${index}]`;
  const artifactErrors = [];
  if (typeof artifact?.name !== "string" || !artifactNamePattern.test(artifact.name)) {
    artifactErrors.push("name must be a stable artifact identifier");
  }
  if (typeof artifact?.sha256 !== "string" || !hex64Pattern.test(artifact.sha256)) {
    artifactErrors.push("sha256 must be lowercase 64-character hex");
  }
  if (!Number.isSafeInteger(artifact?.sizeBytes) || artifact.sizeBytes < 0) {
    artifactErrors.push("sizeBytes must be a non-negative safe integer");
  }

  const path = resolveArtifact(root, artifact?.filename, artifactErrors);
  let actual = null;
  if (path) {
    const bytes = readFileSync(path);
    actual = {
      bytes: bytes.length,
      sha256: sha256(bytes),
    };
    if (artifact.sizeBytes !== actual.bytes) {
      artifactErrors.push("sizeBytes does not match artifact bytes");
    }
    if (artifact.sha256 !== actual.sha256) {
      artifactErrors.push("sha256 does not match artifact bytes");
    }
  }

  assertNoPrivateStrings(artifact, label, artifactErrors);
  errors.push(...artifactErrors.map((error) => `${label}: ${error}`));

  return {
    name: artifact?.name ?? null,
    filename: artifact?.filename ?? null,
    ok: artifactErrors.length === 0,
    expected_sha256: artifact?.sha256 ?? null,
    actual_sha256: actual?.sha256 ?? null,
    expected_size_bytes: artifact?.sizeBytes ?? null,
    actual_size_bytes: actual?.bytes ?? null,
    errors: artifactErrors,
  };
}

function verifySubject(subject, artifactRoot, errors) {
  if (!subject || typeof subject !== "object") {
    errors.push("subject file must contain an object");
    return [];
  }
  if (subject.schema !== subjectSchema) {
    errors.push(`schema must be ${subjectSchema}`);
  }
  if (!subject.subject || typeof subject.subject !== "object") {
    errors.push("subject must be present");
    return [];
  }

  const payload = subject.subject;
  if (payload.kind !== "release_bundle") {
    errors.push("subject.kind must be release_bundle");
  }
  if (typeof payload.repository !== "string" || !repositoryPattern.test(payload.repository)) {
    errors.push("subject.repository must be owner/name");
  }
  if (typeof payload.releaseId !== "string" || !releaseIdPattern.test(payload.releaseId)) {
    errors.push("subject.releaseId contains unsupported characters");
  }
  if (typeof payload.commit !== "string" || !hex40Pattern.test(payload.commit)) {
    errors.push("subject.commit must be a 40-character lowercase hex SHA");
  }
  if (!Array.isArray(payload.artifacts) || payload.artifacts.length === 0) {
    errors.push("subject.artifacts must be a non-empty array");
    return [];
  }

  assertNoPrivateStrings(subject, "subject", errors);

  const seen = new Set();
  return payload.artifacts.map((artifact, index) => {
    if (typeof artifact?.name === "string") {
      if (seen.has(artifact.name)) {
        errors.push(`artifacts[${index}]: duplicate artifact name`);
      }
      seen.add(artifact.name);
    }
    return verifyArtifact(artifactRoot, artifact, index, errors);
  });
}

const args = parseArgs(process.argv.slice(2));
const subjectPath = resolve(repoRoot, args.subject);
const artifactRoot = resolve(repoRoot, args.artifactRoot);
const errors = [];

if (!existsSync(subjectPath)) {
  errors.push(`subject file is missing: ${displayPath(args.subject, subjectPath)}`);
}
if (!existsSync(artifactRoot)) {
  errors.push(`artifact root is missing: ${displayPath(args.artifactRoot, artifactRoot)}`);
}

const subject = existsSync(subjectPath) ? readJson(subjectPath, errors) : null;
const artifacts = existsSync(artifactRoot) ? verifySubject(subject, artifactRoot, errors) : [];

const report = {
  kind: "covenant_release_artifact_subject",
  schema: reportSchema,
  generated_at: new Date().toISOString(),
  subject_schema: subjectSchema,
  ok: errors.length === 0,
  release: subject?.subject
    ? {
        repository: subject.subject.repository ?? null,
        release_id: subject.subject.releaseId ?? null,
        tag: subject.subject.tag ?? null,
        commit: subject.subject.commit ?? null,
      }
    : null,
  artifact_root: displayPath(args.artifactRoot, artifactRoot),
  artifacts,
  errors,
  non_goals: [
    "signing release artifacts",
    "publishing artifacts",
    "uploading artifacts",
    "writing transparency-log entries",
  ],
};

if (args.json) {
  console.log(JSON.stringify(report, null, 2));
} else if (report.ok) {
  console.log("release-artifact-subject: ok");
  for (const artifact of report.artifacts) {
    console.log(`- ok: ${artifact.name}`);
  }
} else {
  console.error("release-artifact-subject: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
}

if (!report.ok) {
  process.exit(1);
}
