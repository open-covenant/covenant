#!/usr/bin/env node
import { chmodSync, existsSync, readFileSync, statSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

const marker = "# covenant-managed-pre-push";

const usage = () => {
  console.error("usage: scripts/install-git-hooks.mjs [--dry-run] [--check] [--force]");
};

let dryRun = false;
let check = false;
let force = false;

for (const arg of process.argv.slice(2)) {
  if (arg === "--dry-run") {
    dryRun = true;
    continue;
  }
  if (arg === "--check") {
    check = true;
    continue;
  }
  if (arg === "--force") {
    force = true;
    continue;
  }
  usage();
  process.exit(2);
}

const git = (args) =>
  spawnSync("git", args, {
    encoding: "utf8"
  });

const gitText = (args) => {
  const result = git(args);
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout.trim();
};

const gitMaybe = (args) => {
  const result = git(args);
  if (result.status !== 0) {
    return null;
  }
  return result.stdout.trim();
};

const repoRoot = gitText(["rev-parse", "--show-toplevel"]);
const hooksPath = "hooks";
const hookPath = join(repoRoot, hooksPath, "pre-push");

const hookExists = () => existsSync(hookPath) && readFileSync(hookPath, "utf8").includes(marker);
const hookExecutable = () => (statSync(hookPath).mode & 0o111) !== 0;
const currentHooksPath = () => gitMaybe(["config", "--get", "core.hooksPath"]);
const installed = () => currentHooksPath() === hooksPath && hookExists() && hookExecutable();

if (check) {
  if (!installed()) {
    console.error("install-git-hooks: repo hooks are not installed");
    process.exit(1);
  }
  console.log(`install-git-hooks: ok (${hookPath})`);
  process.exit(0);
}

if (dryRun) {
  console.log(`install-git-hooks: would set core.hooksPath to ${hooksPath}`);
  console.log(`install-git-hooks: would ensure pre-push hook is executable at ${hookPath}`);
  console.log(`install-git-hooks: repository root ${repoRoot}`);
  process.exit(0);
}

if (!hookExists()) {
  console.error(`install-git-hooks: managed pre-push hook is missing at ${hookPath}`);
  process.exit(1);
}

const configured = currentHooksPath();
if (configured && configured !== hooksPath && !force) {
  console.error(`install-git-hooks: refusing to replace core.hooksPath=${configured}`);
  console.error("Re-run with --force only after reviewing the existing hook configuration.");
  process.exit(1);
}

const config = git(["config", "core.hooksPath", hooksPath]);
if (config.status !== 0) {
  console.error(`install-git-hooks: git config failed: ${(config.stderr || config.stdout).trim()}`);
  process.exit(1);
}
chmodSync(hookPath, 0o755);
console.log(`install-git-hooks: installed repo hooks at ${hookPath}`);
