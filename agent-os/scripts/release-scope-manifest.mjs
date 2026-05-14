#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const manifestSchema = "covenant.release-scope.v1";
const reportSchema = "covenant.release-scope-manifest-check.v1";

const allowedTopLevelKeys = new Set(["schema", "generatedAt", "release", "scope", "non_claims"]);
const allowedReleaseKeys = new Set(["repository", "releaseId", "tag", "commit", "previousTag"]);
const allowedScopeKeys = new Set(["task_count", "task_state_distribution", "task_set_sha256"]);

const workflowStates = new Set([
  "proposed",
  "triaged",
  "planned",
  "in_progress",
  "self_review",
  "cross_review",
  "validation",
  "repair",
  "ready",
  "integrated",
  "blocked",
]);

const requiredNonClaims = [
  "task_set_sha256 binds the canonical concatenation of in-scope task records; it does not expose ids, titles, or transition notes",
  "task_set_sha256 does not claim that every task was reviewed by a human",
  "task_set_sha256 does not claim that every task was security-relevant or release-grade individually",
  "task_count and task_state_distribution are aggregate descriptors only",
];

const repositoryPattern = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const releaseIdPattern = /^[A-Za-z0-9_.:@/-]+$/;
const tagPattern = /^[A-Za-z0-9_.:@/-]+$/;
const hex40Pattern = /^[0-9a-f]{40}$/;
const hex64Pattern = /^[0-9a-f]{64}$/;
const isoTimestampPattern = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/;
const forbiddenPatterns = [
  /\/Users\//,
  /\/home\/[^/\s"]+/,
  /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/,
];

function usage() {
  console.log(`usage: release-scope-manifest --path file [--json]

Read-only schema and internal-consistency check for a covenant.release-scope.v1
manifest. Does not recompute task_set_sha256 (the task corpus is internal-loop
only) and does not sign, publish, or upload the manifest.`);
}

function failUsage() {
  usage();
  process.exit(2);
}

function parseArgs(argv) {
  const args = { path: "", json: false };
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
    if (arg === "--path") {
      args.path = argv[index + 1] ?? "";
      index += 1;
      continue;
    }
    failUsage();
  }
  if (!args.path) {
    failUsage();
  }
  return args;
}

function displayPath(input, absolute) {
  if (absolute.startsWith(`${repoRoot}${sep}`)) {
    return relative(repoRoot, absolute) || ".";
  }
  return input.startsWith("/") ? "<external>" : input;
}

function readJson(path, errors) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    errors.push(`manifest: cannot parse JSON: ${error.message}`);
    return null;
  }
}

function assertNoPrivateStrings(value, label, errors) {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  for (const pattern of forbiddenPatterns) {
    if (pattern.test(text)) {
      errors.push(`${label} contains a forbidden local identity pattern`);
    }
  }
}

function checkIsoTimestamp(value, label, errors) {
  if (typeof value !== "string" || !isoTimestampPattern.test(value)) {
    errors.push(`${label} must be an ISO-8601 UTC timestamp`);
    return;
  }
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) {
    errors.push(`${label} must be a parseable ISO-8601 timestamp`);
  }
}

function checkRelease(release, errors) {
  if (!release || typeof release !== "object" || Array.isArray(release)) {
    errors.push("release must be an object");
    return;
  }
  for (const key of Object.keys(release)) {
    if (!allowedReleaseKeys.has(key)) {
      errors.push(`release contains unknown field: ${key}`);
    }
  }
  if (typeof release.repository !== "string" || !repositoryPattern.test(release.repository)) {
    errors.push("release.repository must be owner/name");
  }
  if (typeof release.releaseId !== "string" || release.releaseId === "" || !releaseIdPattern.test(release.releaseId)) {
    errors.push("release.releaseId must be a non-empty stable identifier");
  }
  if (typeof release.tag !== "string" || release.tag === "" || !tagPattern.test(release.tag)) {
    errors.push("release.tag must be a non-empty git tag");
  }
  if (typeof release.commit !== "string" || !hex40Pattern.test(release.commit)) {
    errors.push("release.commit must be a 40-character lowercase hex SHA");
  }
  if (release.previousTag !== null) {
    if (typeof release.previousTag !== "string" || release.previousTag === "" || !tagPattern.test(release.previousTag)) {
      errors.push("release.previousTag must be null or a non-empty git tag");
    }
  }
}

function checkScope(scope, errors) {
  if (!scope || typeof scope !== "object" || Array.isArray(scope)) {
    errors.push("scope must be an object");
    return null;
  }
  for (const key of Object.keys(scope)) {
    if (!allowedScopeKeys.has(key)) {
      errors.push(`scope contains unknown field: ${key}`);
    }
  }
  if (!Number.isSafeInteger(scope.task_count) || scope.task_count < 0) {
    errors.push("scope.task_count must be a non-negative safe integer");
  }
  if (typeof scope.task_set_sha256 !== "string" || !hex64Pattern.test(scope.task_set_sha256)) {
    errors.push("scope.task_set_sha256 must be lowercase 64-character hex");
  }

  const distribution = scope.task_state_distribution;
  if (!distribution || typeof distribution !== "object" || Array.isArray(distribution)) {
    errors.push("scope.task_state_distribution must be an object");
    return null;
  }
  let sum = 0;
  let distributionOk = true;
  for (const [state, count] of Object.entries(distribution)) {
    if (!workflowStates.has(state)) {
      errors.push(`scope.task_state_distribution contains unknown workflow state: ${state}`);
      distributionOk = false;
      continue;
    }
    if (!Number.isSafeInteger(count) || count < 0) {
      errors.push(`scope.task_state_distribution.${state} must be a non-negative safe integer`);
      distributionOk = false;
      continue;
    }
    sum += count;
  }
  if (distributionOk && Number.isSafeInteger(scope.task_count) && sum !== scope.task_count) {
    errors.push(
      `scope.task_count (${scope.task_count}) does not equal task_state_distribution sum (${sum})`,
    );
  }
  return {
    task_count: scope.task_count ?? null,
    distribution_sum: distributionOk ? sum : null,
    distribution_states: Object.keys(distribution),
  };
}

function checkNonClaims(nonClaims, errors) {
  if (!Array.isArray(nonClaims)) {
    errors.push("non_claims must be an array of strings");
    return;
  }
  for (const entry of nonClaims) {
    if (typeof entry !== "string") {
      errors.push("non_claims entries must be strings");
      return;
    }
  }
  const seen = new Set(nonClaims);
  for (const required of requiredNonClaims) {
    if (!seen.has(required)) {
      errors.push(`non_claims is missing required entry: ${required}`);
    }
  }
}

function checkManifest(manifest, errors) {
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)) {
    errors.push("manifest must be a JSON object");
    return null;
  }
  for (const key of Object.keys(manifest)) {
    if (!allowedTopLevelKeys.has(key)) {
      errors.push(`manifest contains unknown top-level field: ${key}`);
    }
  }
  if (manifest.schema !== manifestSchema) {
    errors.push(`schema must be ${manifestSchema}`);
  }
  checkIsoTimestamp(manifest.generatedAt, "generatedAt", errors);
  checkRelease(manifest.release, errors);
  const scopeSummary = checkScope(manifest.scope, errors);
  checkNonClaims(manifest.non_claims, errors);
  assertNoPrivateStrings(manifest, "manifest", errors);
  return scopeSummary;
}

const args = parseArgs(process.argv.slice(2));
const manifestPath = resolve(repoRoot, args.path);
const errors = [];

if (!existsSync(manifestPath)) {
  errors.push(`manifest file is missing: ${displayPath(args.path, manifestPath)}`);
}

const manifest = existsSync(manifestPath) ? readJson(manifestPath, errors) : null;
const scopeSummary = manifest ? checkManifest(manifest, errors) : null;

const report = {
  kind: "covenant_release_scope_manifest_check",
  schema: reportSchema,
  generated_at: new Date().toISOString(),
  manifest_schema: manifestSchema,
  manifest_path: displayPath(args.path, manifestPath),
  ok: errors.length === 0,
  release: manifest?.release
    ? {
        repository: manifest.release.repository ?? null,
        release_id: manifest.release.releaseId ?? null,
        tag: manifest.release.tag ?? null,
        commit: manifest.release.commit ?? null,
        previous_tag: manifest.release.previousTag ?? null,
      }
    : null,
  scope: scopeSummary,
  errors,
  non_goals: [
    "recomputing task_set_sha256",
    "publishing manifests",
    "signing manifests",
    "writing transparency-log entries",
  ],
};

if (args.json) {
  console.log(JSON.stringify(report, null, 2));
} else if (report.ok) {
  console.log("release-scope-manifest: ok");
  if (report.release) {
    console.log(`- release: ${report.release.repository}@${report.release.tag} (${report.release.commit})`);
  }
  if (report.scope) {
    console.log(`- scope: ${report.scope.task_count} tasks across ${report.scope.distribution_states.length} state(s)`);
  }
} else {
  console.error("release-scope-manifest: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
}

if (!report.ok) {
  process.exit(1);
}
