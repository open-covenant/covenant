#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const script = "agent-os/scripts/autonomy-next.mjs";

function run(args) {
  return spawnSync(process.execPath, [script, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

const errors = [];
const fail = (message) => errors.push(message);

const syntax = spawnSync(process.execPath, ["--check", script], {
  cwd: repoRoot,
  encoding: "utf8",
  stdio: ["ignore", "pipe", "pipe"],
});
if (syntax.status !== 0) {
  fail(`syntax check failed: ${syntax.stderr || syntax.stdout}`);
}

for (const flag of ["--help", "-h"]) {
  const result = run([flag]);
  if (result.status !== 0) {
    fail(`${flag} must exit 0`);
    continue;
  }
  if (!result.stdout.includes("usage: node agent-os/scripts/autonomy-next.mjs")) {
    fail(`${flag} must print usage`);
  }
  if (!result.stdout.includes("--json") || !result.stdout.includes("--seed")) {
    fail(`${flag} must document supported flags`);
  }
}

const unknown = run(["--bogus"]);
if (unknown.status !== 2) {
  fail("unknown flags must exit 2");
}
if (!unknown.stderr.includes("unknown argument")) {
  fail("unknown flags must print an explicit error");
}
if (!unknown.stderr.includes("usage: node agent-os/scripts/autonomy-next.mjs")) {
  fail("unknown flags must print usage to stderr");
}

const json = run(["--json"]);
if (json.status !== 0) {
  fail(`--json must exit 0: ${json.stderr || json.stdout}`);
} else {
  try {
    const output = JSON.parse(json.stdout);
    if (!("next" in output)) {
      fail("--json output must include next");
    }
    if (!Array.isArray(output.candidates)) {
      fail("--json output must include candidates array");
    }
    if (!("seeded" in output)) {
      fail("--json output must include seeded");
    }
  } catch (error) {
    fail(`--json output must parse as JSON: ${error.message}`);
  }
}

if (errors.length > 0) {
  console.error("validate-autonomy-next-cli: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-autonomy-next-cli: ok");
