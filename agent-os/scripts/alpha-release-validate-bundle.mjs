#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const releasesDir = join(repoRoot, "docs", "releases");

function usage() {
  console.log(`usage: alpha-release-validate-bundle <release-id> [--allow-dirty] [--allow-pending] [--allow-draft]

Validate a local alpha release evidence bundle under docs/releases/<release-id>/.

By default this is an acceptance gate: dirty evidence, pending gate results, and draft decisions fail.`);
}

function fail(message, code = 1) {
  console.error(`alpha-release-validate-bundle: ${message}`);
  process.exit(code);
}

function parseArgs(argv) {
  const args = {
    releaseId: "",
    allowDirty: false,
    allowPending: false,
    allowDraft: false,
  };
  for (const arg of argv) {
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    if (arg === "--allow-dirty") {
      args.allowDirty = true;
      continue;
    }
    if (arg === "--allow-pending") {
      args.allowPending = true;
      continue;
    }
    if (arg === "--allow-draft") {
      args.allowDraft = true;
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

function readJson(path, errors) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    errors.push(`${path}: cannot parse JSON: ${error.message}`);
    return null;
  }
}

function scanText(label, text, errors) {
  const forbidden = [
    [/\/Users\/[^\s"')\]]+/, "machine-local home path"],
    [new RegExp(`Co-${"Authored-By"}:`, "i"), "commit attribution trailer"],
    [new RegExp(`${"Generated"} with`, "i"), "AI generation attribution"],
  ];
  for (const [pattern, name] of forbidden) {
    if (pattern.test(text)) {
      errors.push(`${label}: contains forbidden ${name}`);
    }
  }
}

function validateEvidence(data, options, errors) {
  if (!data) return;
  if (data.kind !== "alpha_release_evidence") {
    errors.push("evidence.json: kind must be alpha_release_evidence");
  }
  if (!/^\d{4}-\d{2}-\d{2}T/.test(data.generated_at || "")) {
    errors.push("evidence.json: generated_at must be ISO-like");
  }
  if (!/^[0-9a-f]{40}$/.test(data.commit || "")) {
    errors.push("evidence.json: commit must be a 40-character lowercase hex SHA");
  }
  if (typeof data.commit_short !== "string" || data.commit_short.trim() === "") {
    errors.push("evidence.json: commit_short must be present");
  }
  if (typeof data.branch !== "string" || data.branch.trim() === "") {
    errors.push("evidence.json: branch must be present");
  }
  if (!Number.isInteger(data.dirty_files) || data.dirty_files < 0) {
    errors.push("evidence.json: dirty_files must be a non-negative integer");
  } else if (data.dirty_files !== 0 && !options.allowDirty) {
    errors.push("evidence.json: dirty_files must be 0 for accepted release evidence");
  }
  if (!Array.isArray(data.commands) || data.commands.length === 0) {
    errors.push("evidence.json: commands must be a non-empty array");
  }
  if (!Array.isArray(data.notes)) {
    errors.push("evidence.json: notes must be an array");
  }
}

function decisionFrom(notes) {
  const marker = "## Decision";
  const markerIndex = notes.indexOf(marker);
  if (markerIndex === -1) return "";
  return notes
    .slice(markerIndex + marker.length)
    .split("\n")
    .map((line) => line.trim())
    .find((line) => line.length > 0) || "";
}

function validateNotes(releaseId, notes, evidence, options, errors) {
  const requiredSections = [
    `# ${releaseId} Validation Notes`,
    "## Required Gates",
    "## Live Prerequisites",
    "## Release Notes",
    "## Decision",
  ];
  for (const section of requiredSections) {
    if (!notes.includes(section)) {
      errors.push(`validation.md: missing ${section}`);
    }
  }
  if (notes.includes("result: pending") && !options.allowPending) {
    errors.push("validation.md: pending gate results are not accepted release evidence");
  }
  const decision = decisionFrom(notes);
  const validDecisions = new Set(options.allowDraft
    ? ["draft", "accepted", "rejected", "superseded"]
    : ["accepted", "rejected", "superseded"]);
  if (!validDecisions.has(decision)) {
    errors.push(`validation.md: decision must be one of: ${[...validDecisions].join(", ")}`);
  }
  for (const command of evidence?.commands || []) {
    if (!notes.includes(`\`${command}\``)) {
      errors.push(`validation.md: missing command from evidence.json: ${command}`);
    }
  }
}

const options = parseArgs(process.argv.slice(2));
if (!options.releaseId) {
  usage();
  process.exit(2);
}

validateReleaseId(options.releaseId);

const bundleDir = resolve(releasesDir, options.releaseId);
if (!bundleDir.startsWith(`${releasesDir}${sep}`)) {
  fail("release-id resolves outside docs/releases", 2);
}

const evidencePath = join(bundleDir, "evidence.json");
const notesPath = join(bundleDir, "validation.md");
const errors = [];

if (!existsSync(bundleDir)) {
  errors.push(`docs/releases/${options.releaseId}: bundle directory does not exist`);
}
if (!existsSync(evidencePath)) {
  errors.push(`docs/releases/${options.releaseId}/evidence.json: missing`);
}
if (!existsSync(notesPath)) {
  errors.push(`docs/releases/${options.releaseId}/validation.md: missing`);
}

const evidence = existsSync(evidencePath) ? readJson(evidencePath, errors) : null;
const notes = existsSync(notesPath) ? readFileSync(notesPath, "utf8") : "";

scanText("evidence.json", JSON.stringify(evidence || {}), errors);
scanText("validation.md", notes, errors);
validateEvidence(evidence, options, errors);
validateNotes(options.releaseId, notes, evidence, options, errors);

if (errors.length > 0) {
  console.error("alpha-release-validate-bundle: failed");
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}

console.log(`alpha-release-validate-bundle: ok (${options.releaseId})`);
