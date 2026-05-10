#!/usr/bin/env node
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, resolve } from "node:path";

const manifestRel = "share/covenant/install-manifest.json";

function usage() {
  console.log(`usage: source-install-rollback --prefix path [--backup-id id] [--apply] [--json]

Restore files from the rollback checkpoint recorded in the current source install
manifest. Default mode is a dry run. Use --apply to copy files back and write
local rollback evidence under share/covenant/rollback-reports.`);
}

let prefixInput = "";
let backupId = "";
let apply = false;
let json = false;

const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--prefix") {
    prefixInput = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--backup-id") {
    backupId = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--apply") {
    apply = true;
    continue;
  }
  if (arg === "--json") {
    json = true;
    continue;
  }
  if (arg === "--help" || arg === "-h") {
    usage();
    process.exit(0);
  }
  usage();
  process.exit(2);
}

if (!prefixInput) {
  console.error("source-install-rollback: --prefix is required");
  process.exit(2);
}

const prefix = resolve(prefixInput);

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function resolveInsidePrefix(path) {
  if (!path || isAbsolute(path) || path.split("/").includes("..")) {
    throw new Error(`path is not relative and bounded: ${path}`);
  }
  return join(prefix, path);
}

function readManifest() {
  const path = resolveInsidePrefix(manifestRel);
  if (!existsSync(path)) {
    throw new Error(`missing install manifest: ${manifestRel}`);
  }
  return JSON.parse(readFileSync(path, "utf8"));
}

function checkpointFromManifest(manifest) {
  const checkpoint = manifest.rollback_checkpoint;
  if (!checkpoint || checkpoint.schema !== "covenant.source-install.rollback-checkpoint.v1") {
    throw new Error("current install manifest does not contain a rollback checkpoint");
  }
  if (backupId && checkpoint.id !== backupId) {
    throw new Error(`rollback checkpoint mismatch: expected ${backupId}, found ${checkpoint.id}`);
  }
  if (!Array.isArray(checkpoint.files) || checkpoint.files.length === 0) {
    throw new Error("rollback checkpoint has no files");
  }
  return checkpoint;
}

function operationFor(file) {
  const destination = resolveInsidePrefix(file.path);
  const backup = resolveInsidePrefix(file.backup_path);
  const errors = [];
  let backupSha = null;
  let backupBytes = null;

  if (!existsSync(backup)) {
    errors.push("backup file is missing");
  } else {
    const stat = statSync(backup);
    if (!stat.isFile()) {
      errors.push("backup path is not a regular file");
    } else {
      backupBytes = stat.size;
      backupSha = sha256(backup);
      if (backupSha !== file.sha256) {
        errors.push("backup digest does not match checkpoint");
      }
    }
  }

  return {
    type: file.type,
    path: file.path,
    backup_path: file.backup_path,
    destination_exists: existsSync(destination),
    expected_sha256: file.sha256,
    backup_sha256: backupSha,
    backup_bytes: backupBytes,
    mode: file.mode ?? null,
    ready: errors.length === 0,
    errors,
  };
}

function buildPlan() {
  const manifest = readManifest();
  const checkpoint = checkpointFromManifest(manifest);
  const operations = checkpoint.files.map(operationFor);
  const errors = operations.flatMap((operation) =>
    operation.errors.map((error) => `${operation.path}: ${error}`),
  );

  return {
    kind: "covenant_source_install_rollback_plan",
    schema: "covenant.source-install-rollback-plan.v1",
    generated_at: new Date().toISOString(),
    dry_run: !apply,
    prefix,
    checkpoint: {
      id: checkpoint.id,
      created_at: checkpoint.created_at,
      backup_dir: checkpoint.backup_dir,
      file_count: checkpoint.files.length,
    },
    ready: errors.length === 0,
    operations,
    errors,
  };
}

function applyPlan(plan) {
  if (!plan.ready) {
    throw new Error(`rollback checkpoint is not ready: ${plan.errors.join("; ")}`);
  }

  for (const operation of plan.operations) {
    const destination = resolveInsidePrefix(operation.path);
    const backup = resolveInsidePrefix(operation.backup_path);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(backup, destination);
    if (operation.mode) {
      chmodSync(destination, Number.parseInt(operation.mode, 8));
    }
  }

  const evidenceRel = join("share", "covenant", "rollback-reports", `${plan.checkpoint.id}.json`);
  const evidencePath = resolveInsidePrefix(evidenceRel);
  mkdirSync(dirname(evidencePath), { recursive: true });
  const evidence = {
    ...plan,
    dry_run: false,
    applied_at: new Date().toISOString(),
    evidence_path: evidenceRel,
  };
  writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  return evidence;
}

try {
  const plan = buildPlan();
  const output = apply ? applyPlan(plan) : plan;

  if (json) {
    console.log(JSON.stringify(output, null, 2));
  } else {
    console.log(`source install rollback: ${output.ready ? "ready" : "blocked"}`);
    console.log(`checkpoint: ${output.checkpoint.id}`);
    for (const error of output.errors ?? []) {
      console.log(`blocker: ${error}`);
    }
    if (apply) {
      console.log(`evidence: ${output.evidence_path}`);
    }
  }

  if (!output.ready) {
    process.exitCode = 1;
  }
} catch (error) {
  console.error(`source-install-rollback: ${error.message}`);
  process.exit(1);
}
