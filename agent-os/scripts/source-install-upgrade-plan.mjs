#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const manifestRel = "share/covenant/install-manifest.json";

function usage() {
  console.log(`usage: source-install-upgrade-plan --prefix path [--profile release|debug] [--json]

Inspect an existing source install before running install-source.mjs. The report is
read-only: it does not build, copy, remove, sign, publish, or roll back files.`);
}

let prefixInput = "";
let profile = "release";
let json = false;

const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--prefix") {
    prefixInput = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--profile") {
    profile = args[index + 1] ?? "";
    index += 1;
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
  console.error("source-install-upgrade-plan: --prefix is required");
  process.exit(2);
}

if (!["debug", "release"].includes(profile)) {
  console.error("source-install-upgrade-plan: --profile must be debug or release");
  process.exit(2);
}

const prefix = resolve(prefixInput);

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function installDryRun() {
  const result = spawnSync(
    process.execPath,
    [join(root, "scripts", "install-source.mjs"), "--prefix", prefix, "--profile", profile, "--dry-run", "--json"],
    {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || "install-source dry run failed");
  }

  return JSON.parse(result.stdout);
}

function readExistingManifest() {
  const path = join(prefix, manifestRel);
  if (!existsSync(path)) {
    return {
      present: false,
      valid: false,
      path: manifestRel,
      errors: [],
      binaries: [],
    };
  }

  const errors = [];
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    return {
      present: true,
      valid: false,
      path: manifestRel,
      errors: [`manifest is not valid JSON: ${error.message}`],
      binaries: [],
    };
  }

  if (manifest.schema !== "covenant.source-install.v1") {
    errors.push("manifest schema is not covenant.source-install.v1");
  }
  if (!Array.isArray(manifest.binaries)) {
    errors.push("manifest binaries must be an array");
  }

  const binaries = Array.isArray(manifest.binaries)
    ? manifest.binaries.map((binary) => ({
        name: binary.name ?? null,
        crate: binary.crate ?? null,
        installed_path: binary.installed_path ?? null,
        sha256: binary.sha256 ?? null,
        bytes: Number.isInteger(binary.bytes) ? binary.bytes : null,
      }))
    : [];
  const rollbackCheckpoint = manifest.rollback_checkpoint;

  return {
    present: true,
    valid: errors.length === 0,
    path: manifestRel,
    schema: manifest.schema ?? null,
    source_commit: manifest.source_commit ?? null,
    profile: manifest.profile ?? null,
    generated_at: manifest.generated_at ?? null,
    rollback_checkpoint: rollbackCheckpoint
      ? {
          present: true,
          valid: rollbackCheckpoint.schema === "covenant.source-install.rollback-checkpoint.v1",
          id: rollbackCheckpoint.id ?? null,
          backup_dir: rollbackCheckpoint.backup_dir ?? null,
          file_count: Array.isArray(rollbackCheckpoint.files) ? rollbackCheckpoint.files.length : 0,
        }
      : {
          present: false,
          valid: false,
        },
    errors,
    binaries,
  };
}

function destinationInfo(write, manifest) {
  const destination = write.destination;
  if (!destination || isAbsolute(destination) || destination.split("/").includes("..")) {
    return {
      destination,
      type: write.type,
      action: "invalid_destination",
      existing: { present: false },
      errors: ["planned write destination is not relative and bounded"],
    };
  }

  const path = join(prefix, destination);
  if (!existsSync(path)) {
    return {
      destination,
      type: write.type,
      action: "create",
      existing: { present: false },
      errors: [],
    };
  }

  const stat = statSync(path);
  const hash = stat.isFile() ? sha256(path) : null;
  const manifestBinary = manifest.binaries.find((binary) => binary.installed_path === destination);
  const matchesManifest =
    write.type === "binary" && Boolean(manifest.valid && manifestBinary && manifestBinary.sha256 === hash);
  const errors = [];
  if (write.type === "binary" && manifest.present && !matchesManifest) {
    errors.push("existing binary does not match the installed manifest");
  }
  if (write.type === "binary" && !stat.isFile()) {
    errors.push("existing binary destination is not a regular file");
  }

  return {
    destination,
    type: write.type,
    action: write.type === "manifest" ? "replace_manifest" : "replace",
    existing: {
      present: true,
      file: stat.isFile(),
      bytes: stat.size,
      sha256: hash,
      manifest_sha256: manifestBinary?.sha256 ?? null,
      matches_manifest: write.type === "binary" ? matchesManifest : null,
    },
    errors,
  };
}

function classify(destinations, manifest) {
  const present = destinations.filter((destination) => destination.existing.present);
  const drifted = destinations.some((destination) => destination.errors.length > 0);

  if (present.length === 0 && !manifest.present) return "fresh_install";
  if (!manifest.valid || drifted) return "drifted_existing_install";
  if (present.length === destinations.length) return "clean_existing_install";
  return "partial_existing_install";
}

function buildReport() {
  const plan = installDryRun();
  const manifest = readExistingManifest();
  const destinations = (plan.writes ?? []).map((write) => destinationInfo(write, manifest));
  const upgradeState = classify(destinations, manifest);
  const rollbackAvailable =
    upgradeState === "clean_existing_install" && manifest.rollback_checkpoint.present && manifest.rollback_checkpoint.valid;
  const blockers = [];

  if (manifest.present && !manifest.valid) {
    blockers.push("existing install manifest is invalid");
  }
  for (const destination of destinations) {
    for (const error of destination.errors) {
      blockers.push(`${destination.destination}: ${error}`);
    }
  }
  if (upgradeState === "partial_existing_install") {
    blockers.push("existing install is partial; reconcile missing files before upgrade");
  }

  return {
    kind: "covenant_source_install_upgrade_plan",
    schema: "covenant.source-install-upgrade-plan.v1",
    generated_at: new Date().toISOString(),
    prefix,
    profile,
    install_plan_schema: plan.schema,
    upgrade_state: upgradeState,
    ready_for_fresh_install: upgradeState === "fresh_install",
    ready_for_reviewed_source_upgrade: upgradeState === "clean_existing_install",
    ready_for_automatic_rollback: rollbackAvailable,
    existing_manifest: manifest,
    planned_writes: destinations,
    rollback: {
      supported: true,
      available_for_current_install: rollbackAvailable,
      reason: rollbackAvailable
        ? "current source install manifest records a rollback checkpoint"
        : "current source install manifest does not record a restorable rollback checkpoint",
      required_before_automatic_upgrade: [
        "package-manager rollback coverage",
        "cross-release installer migration checks",
        "public rollback policy approval",
      ],
    },
    blockers,
    human_decisions: [
      "public package publication",
      "project signing key custody",
      "automatic rollback policy",
    ],
  };
}

try {
  const report = buildReport();

  if (json) {
    console.log(JSON.stringify(report, null, 2));
  } else {
    console.log(`source install upgrade preflight: ${report.upgrade_state}`);
    console.log(`fresh install: ${report.ready_for_fresh_install ? "ready" : "not ready"}`);
    console.log(
      `reviewed source upgrade: ${report.ready_for_reviewed_source_upgrade ? "ready" : "not ready"}`,
    );
    for (const blocker of report.blockers) {
      console.log(`blocker: ${blocker}`);
    }
    console.log("automatic rollback: blocked");
  }
} catch (error) {
  console.error(`source-install-upgrade-plan: ${error.message}`);
  process.exit(1);
}
