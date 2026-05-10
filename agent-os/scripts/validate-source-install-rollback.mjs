#!/usr/bin/env node
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const targetDir = join(root, "target", "source-install-rollback-fixture", String(process.pid));
const daemonTarget = join(targetDir, "covenantd");
const cliTarget = join(targetDir, "covenant");

function fail(message) {
  console.error(`validate-source-install-rollback: ${message}`);
  process.exit(1);
}

function run(args) {
  const result = spawnSync(process.execPath, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    fail(result.stderr || result.stdout || `${args.join(" ")} failed`);
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    fail(`output is not JSON: ${error.message}`);
  }
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function writeTargets(version) {
  mkdirSync(targetDir, { recursive: true });
  writeFileSync(daemonTarget, `daemon ${version}\n`, { mode: 0o755 });
  writeFileSync(cliTarget, `cli ${version}\n`, { mode: 0o755 });
}

function install(prefix) {
  return run([
    "scripts/install-source.mjs",
    "--prefix",
    prefix,
    "--profile",
    "debug",
    "--skip-build",
    "--artifact-dir",
    relative(root, targetDir),
    "--json",
  ]);
}

function rollback(prefix, apply = false) {
  return run([
    "scripts/source-install-rollback.mjs",
    "--prefix",
    prefix,
    "--json",
    ...(apply ? ["--apply"] : []),
  ]);
}

function upgradePlan(prefix) {
  return run(["scripts/source-install-upgrade-plan.mjs", "--prefix", prefix, "--json"]);
}

try {
  const dir = mkdtempSync(join(tmpdir(), "covenant-source-rollback-"));
  const prefix = join(dir, "prefix");

  writeTargets("v1");
  const first = install(prefix);
  assert(!first.manifest.rollback_checkpoint, "fresh install must not create a rollback checkpoint");

  writeTargets("v2");
  const second = install(prefix);
  const checkpoint = second.manifest.rollback_checkpoint;
  assert(checkpoint?.schema === "covenant.source-install.rollback-checkpoint.v1", "checkpoint schema missing");
  assert(checkpoint.files.length === 3, "checkpoint must preserve two binaries and the prior manifest");
  assert(readFileSync(join(prefix, "bin", "covenantd"), "utf8") === "daemon v2\n", "v2 daemon not installed");

  const preflight = upgradePlan(prefix);
  assert(preflight.ready_for_automatic_rollback === true, "preflight should detect rollback checkpoint");

  const dryRun = rollback(prefix);
  assert(dryRun.kind === "covenant_source_install_rollback_plan", "unexpected rollback plan kind");
  assert(dryRun.schema === "covenant.source-install-rollback-plan.v1", "unexpected rollback schema");
  assert(dryRun.ready === true, "rollback dry-run should be ready");
  assert(dryRun.operations.length === 3, "rollback must include all checkpoint files");

  const applied = rollback(prefix, true);
  assert(applied.ready === true, "rollback apply should be ready");
  assert(applied.evidence_path, "rollback apply must record evidence path");
  assert(readFileSync(join(prefix, "bin", "covenantd"), "utf8") === "daemon v1\n", "daemon was not restored");
  assert(readFileSync(join(prefix, "bin", "covenant"), "utf8") === "cli v1\n", "cli was not restored");
  assert(
    existsSync(join(prefix, applied.evidence_path)),
    "rollback evidence report was not written under the prefix",
  );

  const tamperedPrefix = join(dir, "tampered");
  writeTargets("v1");
  install(tamperedPrefix);
  writeTargets("v2");
  const tamperedInstall = install(tamperedPrefix);
  const backup = tamperedInstall.manifest.rollback_checkpoint.files.find((file) => file.path === "bin/covenantd");
  writeFileSync(join(tamperedPrefix, backup.backup_path), "tampered backup\n");

  const result = spawnSync(
    process.execPath,
    ["scripts/source-install-rollback.mjs", "--prefix", tamperedPrefix, "--json"],
    {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  assert(result.status === 1, "tampered checkpoint should fail dry-run readiness");
  const tampered = JSON.parse(result.stdout);
  assert(
    tampered.errors.some((error) => error.includes("backup digest does not match")),
    "tampered checkpoint digest error missing",
  );
} finally {
  rmSync(targetDir, { recursive: true, force: true });
}

console.log("validate-source-install-rollback: ok");
