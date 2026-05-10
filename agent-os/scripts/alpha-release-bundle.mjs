#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const releasesDir = join(repoRoot, "docs", "releases");
const evidenceScript = join(repoRoot, "agent-os", "scripts", "alpha-release-evidence.mjs");

function usage() {
  console.log(`usage: alpha-release-bundle <release-id> [--dry-run] [--force]

Create a local alpha release evidence bundle under docs/releases/<release-id>/.

This command does not tag, push, publish, or sign release artifacts.`);
}

function fail(message, code = 1) {
  console.error(`alpha-release-bundle: ${message}`);
  process.exit(code);
}

function parseArgs(argv) {
  const args = { releaseId: "", dryRun: false, force: false };
  for (const arg of argv) {
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    if (arg === "--dry-run") {
      args.dryRun = true;
      continue;
    }
    if (arg === "--force") {
      args.force = true;
      continue;
    }
    if (!args.releaseId) {
      args.releaseId = arg;
      continue;
    }
    fail(`unknown argument: ${arg}`, 2);
  }
  return args;
}

function validateReleaseId(releaseId) {
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(releaseId)) {
    fail("release-id must be lowercase letters, digits, dots, underscores, or hyphens", 2);
  }
  if (releaseId.includes("..")) {
    fail("release-id must not contain path traversal", 2);
  }
}

function evidence() {
  const text = execFileSync(process.execPath, [evidenceScript, "--json"], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return JSON.parse(text);
}

function validationNotes(releaseId, data) {
  const commands = data.commands
    .map((command) => `- [ ] \`${command}\` - result: pending`)
    .join("\n");
  const notes = data.notes.map((note) => `- ${note}`).join("\n");
  const readinessBlockers = data.readiness?.blockers?.length
    ? data.readiness.blockers.map((blocker) => `- ${blocker}`).join("\n")
    : "- none";

  return `# ${releaseId} Validation Notes

Status: draft
Generated: ${data.generated_at}
Candidate commit: ${data.commit}
Branch: ${data.branch}
Dirty files: ${data.dirty_files}
Alpha readiness: ${data.readiness?.ready ? "ready" : "blocked"}

## Required Gates

${commands}

## Alpha Readiness

Blockers:

${readinessBlockers}

## Live Prerequisites

- [ ] Record unavailable live prerequisites or mark all live gates executed.

## Release Notes

${notes}

## Decision

draft

Accepted evidence requires dirty files to be 0 and every required gate above to be recorded as passed, failed, or intentionally skipped with a reason.
`;
}

function digestFile(path) {
  const bytes = readFileSync(path);
  return {
    bytes: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function bundleFiles(dir, prefix = "") {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name))) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    const absolutePath = join(dir, entry.name);
    if (path === "manifest.json") {
      continue;
    }
    if (entry.isDirectory()) {
      files.push(...bundleFiles(absolutePath, path));
      continue;
    }
    if (!entry.isFile()) {
      fail(`unsupported bundle entry: ${path}`);
    }
    files.push(path);
  }
  return files;
}

function bundleManifest(releaseId, target, files) {
  return {
    schema: "covenant.alpha-release-manifest.v1",
    kind: "alpha_release_manifest",
    release_id: releaseId,
    files: files
      .map((path) => ({ path, ...digestFile(join(target, path)) }))
      .sort((left, right) => left.path.localeCompare(right.path)),
  };
}

const args = parseArgs(process.argv.slice(2));
if (!args.releaseId) {
  usage();
  process.exit(2);
}

validateReleaseId(args.releaseId);

const target = resolve(releasesDir, args.releaseId);
if (!target.startsWith(`${releasesDir}${sep}`)) {
  fail("release-id resolves outside docs/releases", 2);
}

const evidencePath = join(target, "evidence.json");
const notesPath = join(target, "validation.md");
const manifestPath = join(target, "manifest.json");

if (args.dryRun) {
  console.log(`alpha-release-bundle: would create docs/releases/${args.releaseId}/`);
  console.log("  - evidence.json");
  console.log("  - validation.md");
  console.log("  - manifest.json");
  process.exit(0);
}

if (existsSync(target) && !args.force && readdirSync(target).length > 0) {
  fail(`docs/releases/${args.releaseId} already exists; use --force to refresh scaffold files`, 2);
}

const data = evidence();
mkdirSync(target, { recursive: true });
writeFileSync(evidencePath, `${JSON.stringify(data, null, 2)}\n`);
writeFileSync(notesPath, validationNotes(args.releaseId, data));
writeFileSync(manifestPath, `${JSON.stringify(bundleManifest(args.releaseId, target, bundleFiles(target)), null, 2)}\n`);

console.log(`alpha-release-bundle: wrote docs/releases/${args.releaseId}/`);
console.log("  - evidence.json");
console.log("  - validation.md");
console.log("  - manifest.json");
