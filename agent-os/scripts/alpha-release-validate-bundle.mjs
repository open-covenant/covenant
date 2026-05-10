#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const releasesDir = join(repoRoot, "docs", "releases");
const evidenceSchema = "covenant.alpha-release-evidence.v1";
const manifestSchema = "covenant.alpha-release-manifest.v1";

function usage() {
  console.log(`usage: alpha-release-validate-bundle <release-id> [--allow-dirty] [--allow-pending] [--allow-draft] [--allow-blocked-readiness]

Validate a local alpha release evidence bundle under docs/releases/<release-id>/.

By default this is an acceptance gate: dirty evidence, pending gate results, draft decisions, and blocked readiness fail.`);
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
    allowBlockedReadiness: false,
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
    if (arg === "--allow-blocked-readiness") {
      args.allowBlockedReadiness = true;
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
  if (data.schema !== evidenceSchema) {
    errors.push(`evidence.json: schema must be ${evidenceSchema}`);
  }
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
  validateReadiness(data.readiness, options, errors);
}

function fileDigest(path) {
  const bytes = readFileSync(path);
  return {
    bytes: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function bundleFiles(dir, errors, prefix = "") {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name))) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    const absolutePath = join(dir, entry.name);
    if (path === "manifest.json") {
      continue;
    }
    if (entry.isDirectory()) {
      files.push(...bundleFiles(absolutePath, errors, path));
      continue;
    }
    if (!entry.isFile()) {
      errors.push(`manifest.json: unsupported bundle entry: ${path}`);
      continue;
    }
    files.push(path);
  }
  return files;
}

function validateManifest(releaseId, manifest, bundleDir, errors) {
  if (!manifest) return;
  if (manifest.schema !== manifestSchema) {
    errors.push(`manifest.json: schema must be ${manifestSchema}`);
  }
  if (manifest.kind !== "alpha_release_manifest") {
    errors.push("manifest.json: kind must be alpha_release_manifest");
  }
  if (manifest.release_id !== releaseId) {
    errors.push("manifest.json: release_id must match the validated release id");
  }
  if (!Array.isArray(manifest.files) || manifest.files.length === 0) {
    errors.push("manifest.json: files must be a non-empty array");
    return;
  }

  const seen = new Set();
  const required = new Set(["evidence.json", "validation.md"]);
  const actualFiles = existsSync(bundleDir) ? new Set(bundleFiles(bundleDir, errors)) : new Set();
  for (const entry of manifest.files) {
    if (typeof entry?.path !== "string" || entry.path.trim() === "") {
      errors.push("manifest.json: files[].path must be present");
      continue;
    }
    if (entry.path.startsWith("/") || entry.path.includes("..") || entry.path.includes("\\")) {
      errors.push(`manifest.json: invalid relative file path: ${entry.path}`);
      continue;
    }
    if (seen.has(entry.path)) {
      errors.push(`manifest.json: duplicate file path: ${entry.path}`);
      continue;
    }
    seen.add(entry.path);

    const filePath = resolve(bundleDir, entry.path);
    if (!filePath.startsWith(`${bundleDir}${sep}`)) {
      errors.push(`manifest.json: file path resolves outside bundle: ${entry.path}`);
      continue;
    }
    if (!existsSync(filePath)) {
      errors.push(`manifest.json: missing recorded file: ${entry.path}`);
      continue;
    }
    const digest = fileDigest(filePath);
    if (entry.bytes !== digest.bytes) {
      errors.push(`manifest.json: byte count mismatch for ${entry.path}`);
    }
    if (entry.sha256 !== digest.sha256) {
      errors.push(`manifest.json: sha256 mismatch for ${entry.path}`);
    }
  }

  for (const path of actualFiles) {
    if (!seen.has(path)) {
      errors.push(`manifest.json: missing bundle file digest: ${path}`);
    }
  }

  for (const path of required) {
    if (!seen.has(path)) {
      errors.push(`manifest.json: missing required file digest: ${path}`);
    }
  }
}

function validateReadiness(readiness, options, errors) {
  if (!readiness || typeof readiness !== "object") {
    errors.push("evidence.json: readiness must be present");
    return;
  }
  if (readiness.kind !== "alpha_release_readiness") {
    errors.push("evidence.json: readiness.kind must be alpha_release_readiness");
  }
  if (!/^\d{4}-\d{2}-\d{2}T/.test(readiness.generated_at || "")) {
    errors.push("evidence.json: readiness.generated_at must be ISO-like");
  }
  if (typeof readiness.ready !== "boolean") {
    errors.push("evidence.json: readiness.ready must be boolean");
  }
  if (!Array.isArray(readiness.blockers)) {
    errors.push("evidence.json: readiness.blockers must be an array");
  }
  if (!Array.isArray(readiness.checks) || readiness.checks.length === 0) {
    errors.push("evidence.json: readiness.checks must be a non-empty array");
  } else {
    for (const check of readiness.checks) {
      if (typeof check?.id !== "string" || check.id.trim() === "") {
        errors.push("evidence.json: readiness.checks[].id must be present");
      }
      if (typeof check?.ok !== "boolean") {
        errors.push(`evidence.json: readiness check ${check?.id || "(unknown)"} ok must be boolean`);
      }
      if (typeof check?.command !== "string" || check.command.trim() === "") {
        errors.push(`evidence.json: readiness check ${check?.id || "(unknown)"} command must be present`);
      }
      if ("output" in check) {
        errors.push(`evidence.json: readiness check ${check?.id || "(unknown)"} must not include raw command output`);
      }
    }
  }
  if (readiness.ready === false && readiness.blockers?.length === 0) {
    errors.push("evidence.json: blocked readiness must name at least one blocker");
  }
  if (readiness.ready === true && readiness.blockers?.length > 0) {
    errors.push("evidence.json: ready readiness must not list blockers");
  }
  if (readiness.ready === false && !options.allowBlockedReadiness) {
    errors.push("evidence.json: readiness.ready must be true for accepted release evidence");
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

function metadataValue(notes, label) {
  const prefix = `${label}:`;
  const line = notes.split(/\r?\n/).find((entry) => entry.startsWith(prefix));
  return line ? line.slice(prefix.length).trim() : null;
}

function validateMetadata(notes, evidence, decision, errors) {
  if (!evidence) return;
  const expected = new Map([
    ["Status", decision],
    ["Generated", evidence.generated_at],
    ["Candidate commit", evidence.commit],
    ["Branch", evidence.branch],
    ["Dirty files", String(evidence.dirty_files)],
    ["Alpha readiness", evidence.readiness?.ready ? "ready" : "blocked"],
  ]);
  for (const [label, value] of expected) {
    const actual = metadataValue(notes, label);
    if (actual === null) {
      errors.push(`validation.md: missing ${label}`);
    } else if (actual !== value) {
      errors.push(`validation.md: ${label} must match evidence.json`);
    }
  }
}

function sectionLines(notes, heading) {
  const lines = notes.split(/\r?\n/);
  const start = lines.findIndex((line) => line.trim() === heading);
  if (start === -1) return null;
  const end = lines.findIndex((line, index) => index > start && /^##\s+/.test(line));
  return lines.slice(start + 1, end === -1 ? lines.length : end);
}

function validateGateResult(command, line, options, errors) {
  const match = line.match(/^\s*-\s+\[([ xX])\]\s+`[^`]+`\s+-\s+result:\s*([a-z]+)(?::\s*(.+))?\s*$/i);
  if (!match) {
    errors.push(`validation.md: command must use gate result format: ${command}`);
    return null;
  }

  const checked = match[1].toLowerCase() === "x";
  const result = match[2].toLowerCase();
  const detail = (match[3] || "").trim();
  const validResults = new Set(["passed", "failed", "skipped", "pending"]);
  if (!validResults.has(result)) {
    errors.push(`validation.md: invalid result for command ${command}: ${result}`);
  }
  if (!checked && !options.allowPending) {
    errors.push(`validation.md: command result must be checked for accepted evidence: ${command}`);
  }
  if (result === "pending" && !options.allowPending) {
    errors.push(`validation.md: pending gate result is not accepted release evidence: ${command}`);
  }
  if (result === "skipped" && detail === "") {
    errors.push(`validation.md: skipped gate must include a reason: ${command}`);
  }
  return { command, checked, result, detail };
}

function validateDecisionOutcome(decision, gateResults, errors) {
  if (decision !== "accepted") return;
  const nonPassing = gateResults.filter((gate) => gate.result !== "passed" || !gate.checked);
  for (const gate of nonPassing) {
    errors.push(`validation.md: accepted decision requires passed gate: ${gate.command}`);
  }
}

function validateNotes(releaseId, notes, evidence, options, errors) {
  const requiredSections = [
    `# ${releaseId} Validation Notes`,
    "## Required Gates",
    "## Alpha Readiness",
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
  validateMetadata(notes, evidence, decision, errors);
  const lines = sectionLines(notes, "## Required Gates") || [];
  const gateResults = [];
  for (const command of evidence?.commands || []) {
    const marker = `\`${command}\``;
    const matches = lines.filter((line) => line.includes(marker));
    if (matches.length === 0) {
      errors.push(`validation.md: missing command from evidence.json: ${command}`);
      continue;
    }
    if (matches.length > 1) {
      errors.push(`validation.md: duplicate command from evidence.json: ${command}`);
      continue;
    }
    const result = validateGateResult(command, matches[0], options, errors);
    if (result) {
      gateResults.push(result);
    }
  }
  validateDecisionOutcome(decision, gateResults, errors);
  const readinessLines = sectionLines(notes, "## Alpha Readiness") || [];
  for (const blocker of evidence?.readiness?.blockers || []) {
    if (!readinessLines.some((line) => line.includes(blocker))) {
      errors.push(`validation.md: missing readiness blocker from evidence.json: ${blocker}`);
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
const manifestPath = join(bundleDir, "manifest.json");
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
if (!existsSync(manifestPath)) {
  errors.push(`docs/releases/${options.releaseId}/manifest.json: missing`);
}

const evidence = existsSync(evidencePath) ? readJson(evidencePath, errors) : null;
const notes = existsSync(notesPath) ? readFileSync(notesPath, "utf8") : "";
const manifest = existsSync(manifestPath) ? readJson(manifestPath, errors) : null;

scanText("evidence.json", JSON.stringify(evidence || {}), errors);
scanText("validation.md", notes, errors);
scanText("manifest.json", JSON.stringify(manifest || {}), errors);
validateEvidence(evidence, options, errors);
validateNotes(options.releaseId, notes, evidence, options, errors);
validateManifest(options.releaseId, manifest, bundleDir, errors);

if (errors.length > 0) {
  console.error("alpha-release-validate-bundle: failed");
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}

console.log(`alpha-release-validate-bundle: ok (${options.releaseId})`);
