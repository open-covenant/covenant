#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const manifestSchema = "covenant.release-scope.v1";

const repositoryPattern = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const releaseIdPattern = /^[A-Za-z0-9_.:@/-]+$/;
const tagPattern = /^[A-Za-z0-9_.:@/-]+$/;
const hex40Pattern = /^[0-9a-f]{40}$/;
const idPattern = /^[A-Za-z0-9_.:@/-]+$/;

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

function usage() {
  console.log(`usage: build-release-scope-manifest \\
  --task-dir dir --repository owner/name --release-id id --tag tag \\
  --commit 40hex (--previous-tag tag | --first-release) [--output path]

Build a covenant.release-scope.v1 manifest by canonicalizing every task record
under --task-dir (sorted keys at every level, no whitespace, no trailing
newline), concatenating with \\n, and hashing with SHA-256. Does not sign,
publish, upload, or write transparency-log entries.`);
}

function failUsage(message) {
  if (message) {
    process.stderr.write(`build-release-scope-manifest: ${message}\n`);
  }
  usage();
  process.exit(2);
}

function parseArgs(argv) {
  const args = {
    taskDir: "",
    repository: "",
    releaseId: "",
    tag: "",
    commit: "",
    previousTag: "",
    firstRelease: false,
    output: "",
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    if (arg === "--first-release") {
      args.firstRelease = true;
      continue;
    }
    const next = argv[index + 1];
    switch (arg) {
      case "--task-dir":
        args.taskDir = next ?? "";
        index += 1;
        continue;
      case "--repository":
        args.repository = next ?? "";
        index += 1;
        continue;
      case "--release-id":
        args.releaseId = next ?? "";
        index += 1;
        continue;
      case "--tag":
        args.tag = next ?? "";
        index += 1;
        continue;
      case "--commit":
        args.commit = next ?? "";
        index += 1;
        continue;
      case "--previous-tag":
        args.previousTag = next ?? "";
        index += 1;
        continue;
      case "--output":
        args.output = next ?? "";
        index += 1;
        continue;
      default:
        failUsage(`unknown flag: ${arg}`);
    }
  }
  if (!args.taskDir) failUsage("--task-dir is required");
  if (!args.repository) failUsage("--repository is required");
  if (!args.releaseId) failUsage("--release-id is required");
  if (!args.tag) failUsage("--tag is required");
  if (!args.commit) failUsage("--commit is required");
  if (args.firstRelease && args.previousTag) {
    failUsage("--first-release and --previous-tag are mutually exclusive");
  }
  if (!args.firstRelease && !args.previousTag) {
    failUsage("either --previous-tag or --first-release is required");
  }
  return args;
}

function canonicalize(value) {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalize).join(",")}]`;
  }
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    return `{${keys
      .map((key) => `${JSON.stringify(key)}:${canonicalize(value[key])}`)
      .join(",")}}`;
  }
  throw new Error(`unsupported value type: ${typeof value}`);
}

function loadTaskRecords(taskDir) {
  const absolute = resolve(repoRoot, taskDir);
  if (!existsSync(absolute)) {
    throw new Error(`task directory does not exist: ${taskDir}`);
  }
  const files = readdirSync(absolute).filter((name) => name.endsWith(".json"));
  const records = [];
  for (const name of files) {
    const path = join(absolute, name);
    let parsed;
    try {
      parsed = JSON.parse(readFileSync(path, "utf8"));
    } catch (error) {
      throw new Error(`task record is not valid JSON: ${name}: ${error.message}`);
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new Error(`task record is not an object: ${name}`);
    }
    if (typeof parsed.id !== "string" || !idPattern.test(parsed.id)) {
      throw new Error(`task record is missing a valid id field: ${name}`);
    }
    if (typeof parsed.state !== "string" || !workflowStates.has(parsed.state)) {
      throw new Error(`task record has an unknown state: ${name}: ${parsed.state}`);
    }
    records.push(parsed);
  }
  records.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const ids = new Set();
  for (const record of records) {
    if (ids.has(record.id)) {
      throw new Error(`duplicate task id across records: ${record.id}`);
    }
    ids.add(record.id);
  }
  return records;
}

function buildManifest(args, records) {
  if (!repositoryPattern.test(args.repository)) {
    throw new Error("--repository must be owner/name");
  }
  if (!releaseIdPattern.test(args.releaseId)) {
    throw new Error("--release-id contains unsupported characters");
  }
  if (!tagPattern.test(args.tag)) {
    throw new Error("--tag contains unsupported characters");
  }
  if (!hex40Pattern.test(args.commit)) {
    throw new Error("--commit must be 40-character lowercase hex");
  }
  if (args.previousTag && !tagPattern.test(args.previousTag)) {
    throw new Error("--previous-tag contains unsupported characters");
  }

  const distribution = {};
  for (const record of records) {
    distribution[record.state] = (distribution[record.state] ?? 0) + 1;
  }

  const concatenated = records.map((record) => canonicalize(record)).join("\n");
  const digest = createHash("sha256").update(concatenated, "utf8").digest("hex");

  return {
    schema: manifestSchema,
    generatedAt: new Date().toISOString(),
    release: {
      repository: args.repository,
      releaseId: args.releaseId,
      tag: args.tag,
      commit: args.commit,
      previousTag: args.firstRelease ? null : args.previousTag,
    },
    scope: {
      task_count: records.length,
      task_state_distribution: distribution,
      task_set_sha256: digest,
    },
    non_claims: requiredNonClaims.slice(),
  };
}

const args = parseArgs(process.argv.slice(2));

let records;
try {
  records = loadTaskRecords(args.taskDir);
} catch (error) {
  process.stderr.write(`build-release-scope-manifest: ${error.message}\n`);
  process.exit(1);
}

let manifest;
try {
  manifest = buildManifest(args, records);
} catch (error) {
  process.stderr.write(`build-release-scope-manifest: ${error.message}\n`);
  process.exit(1);
}

const serialized = `${JSON.stringify(manifest, null, 2)}\n`;
if (args.output) {
  const outputPath = resolve(repoRoot, args.output);
  writeFileSync(outputPath, serialized);
} else {
  process.stdout.write(serialized);
}
