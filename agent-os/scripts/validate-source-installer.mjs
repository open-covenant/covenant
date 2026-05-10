#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const prefix = join(tmpdir(), "covenant-source-install-validation");

const result = spawnSync(
  process.execPath,
  [join(root, "scripts", "install-source.mjs"), "--prefix", prefix, "--dry-run", "--json"],
  {
    cwd: root,
    encoding: "utf8",
  },
);

if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

let plan;
try {
  plan = JSON.parse(result.stdout);
} catch (error) {
  console.error(`validate-source-installer: dry-run output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (plan.kind !== "covenant_source_install_plan") {
  fail("unexpected plan kind");
}
if (plan.schema !== "covenant.source-install.v1") {
  fail("unexpected plan schema");
}
if (plan.dry_run !== true) {
  fail("dry_run must be true");
}
if (!Array.isArray(plan.build?.command) || plan.build.command[0] !== "cargo") {
  fail("build command must be a cargo argv array");
}
if (plan.build?.cwd !== "agent-os") {
  fail("build cwd must stay repository-relative");
}
if (!Array.isArray(plan.writes) || plan.writes.length !== 3) {
  fail("plan must include two binaries and one manifest write");
}

const destinations = new Set();
for (const write of plan.writes ?? []) {
  if (!write.destination || isAbsolute(write.destination) || write.destination.includes("..")) {
    fail(`write destination must be relative and bounded: ${write.destination}`);
  }
  destinations.add(write.destination);
}
for (const required of [
  "bin/covenantd",
  "bin/covenant",
  "share/covenant/install-manifest.json",
]) {
  if (!destinations.has(required)) {
    fail(`missing destination: ${required}`);
  }
}

if (errors.length > 0) {
  console.error("validate-source-installer: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-source-installer: ok");
