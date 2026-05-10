#!/usr/bin/env node
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const summaryScript = join(here, "autonomy-summary.mjs");
const publishScript = join(here, "autonomy-publish-summary.mjs");

function usage() {
  console.log(`usage: validate-autonomy-summary

Run read-only checks for the autonomy summary generator.`);
}

function run(args) {
  const result = spawnSync(process.execPath, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trimEnd(),
    stderr: (result.stderr || "").trimEnd(),
  };
}

function parseJson(label, text, errors) {
  try {
    return JSON.parse(text);
  } catch (error) {
    errors.push(`${label}: invalid JSON: ${error.message}`);
    return null;
  }
}

function fail(errors) {
  console.error("validate-autonomy-summary: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

function assertSummary(summary, label, errors, limit) {
  if (summary?.schema !== "covenant.autonomy-summary.v1") {
    errors.push(`${label}: schema mismatch`);
  }
  if (!summary?.totals || typeof summary.totals !== "object") {
    errors.push(`${label}: totals object missing`);
    return;
  }
  for (const key of ["tasks", "allTasks", "events", "allEvents"]) {
    if (!Number.isInteger(summary.totals[key]) || summary.totals[key] < 0) {
      errors.push(`${label}: totals.${key} must be a non-negative integer`);
    }
  }
  if (!summary.totals.byState || typeof summary.totals.byState !== "object") {
    errors.push(`${label}: totals.byState missing`);
  } else {
    const stateTotal = Object.values(summary.totals.byState).reduce((sum, value) => sum + value, 0);
    if (stateTotal !== summary.totals.tasks) {
      errors.push(`${label}: byState total does not match scoped task count`);
    }
  }
  for (const key of ["active", "blocked", "integrated", "recentEvents"]) {
    if (!Array.isArray(summary[key])) {
      errors.push(`${label}: ${key} must be an array`);
    }
  }
  if (Array.isArray(summary.integrated) && summary.integrated.length > limit) {
    errors.push(`${label}: integrated exceeds --limit`);
  }
  if (Array.isArray(summary.recentEvents) && summary.recentEvents.length > limit) {
    errors.push(`${label}: recentEvents exceeds --limit`);
  }
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
if (args.length > 0) {
  usage();
  process.exit(2);
}

const errors = [];

const markdown = run([summaryScript, "--limit", "3"]);
if (markdown.status !== 0) {
  errors.push(`markdown summary failed: ${markdown.stderr || markdown.stdout}`);
} else {
  if (!markdown.stdout.startsWith("# Autonomy Summary")) {
    errors.push("markdown summary must start with title");
  }
  for (const heading of ["## Active", "## Blocked", "## Recently Integrated", "## Recent Events"]) {
    if (!markdown.stdout.includes(heading)) {
      errors.push(`markdown summary missing heading: ${heading}`);
    }
  }
}

const json = run([summaryScript, "--format", "json", "--limit", "5"]);
if (json.status !== 0) {
  errors.push(`json summary failed: ${json.stderr || json.stdout}`);
}
const summary = parseJson("json summary", json.stdout, errors);
if (summary) {
  assertSummary(summary, "json summary", errors, 5);
  if (summary.since !== null) {
    errors.push("json summary without --since should report since=null");
  }
  if (summary.totals.tasks !== summary.totals.allTasks) {
    errors.push("json summary without --since should include all tasks");
  }
}

const scopedJson = run([
  summaryScript,
  "--format",
  "json",
  "--since",
  "1970-01-01",
  "--limit",
  "2",
]);
if (scopedJson.status !== 0) {
  errors.push(`scoped json summary failed: ${scopedJson.stderr || scopedJson.stdout}`);
}
const scopedSummary = parseJson("scoped json summary", scopedJson.stdout, errors);
if (scopedSummary) {
  assertSummary(scopedSummary, "scoped json summary", errors, 2);
  if (scopedSummary.since !== "1970-01-01") {
    errors.push("scoped json summary should preserve since date");
  }
}

const invalidLimit = run([summaryScript, "--format", "json", "--limit", "0"]);
if (invalidLimit.status === 0) {
  errors.push("autonomy-summary should reject --limit 0");
}

let tempDir = null;
try {
  mkdirSync(join(repoRoot, "reports"), { recursive: true });
  tempDir = mkdtempSync(join(repoRoot, "reports", "autonomy-summary-validator-"));
  const outPath = join(tempDir, "summary.md");
  const publish = run([
    publishScript,
    "--out",
    outPath,
    "--since",
    "1970-01-01",
    "--limit",
    "2",
  ]);
  if (publish.status !== 0) {
    errors.push(`published summary write failed: ${publish.stderr || publish.stdout}`);
  } else {
    const published = readFileSync(outPath, "utf8");
    if (!published.startsWith("<!-- Generated from autonomy task records.")) {
      errors.push("published summary should start with generated-source comment");
    }
    if (!published.includes("# Autonomy Summary since 1970-01-01")) {
      errors.push("published summary should include scoped Markdown summary");
    }

    const check = run([
      publishScript,
      "--check",
      "--out",
      outPath,
      "--since",
      "1970-01-01",
      "--limit",
      "2",
    ]);
    if (check.status !== 0) {
      errors.push(`published summary check failed: ${check.stderr || check.stdout}`);
    }

    writeFileSync(outPath, `${published}\nmanual drift\n`);
    const drift = run([
      publishScript,
      "--check",
      "--out",
      outPath,
      "--since",
      "1970-01-01",
      "--limit",
      "2",
    ]);
    if (drift.status === 0) {
      errors.push("published summary check should reject drift");
    }
  }

  const publishStdout = run([
    publishScript,
    "--stdout",
    "--since",
    "1970-01-01",
    "--limit",
    "1",
  ]);
  if (publishStdout.status !== 0) {
    errors.push(`published summary stdout failed: ${publishStdout.stderr || publishStdout.stdout}`);
  } else if (!publishStdout.stdout.includes("Validate with: node agent-os/scripts/autonomy-publish-summary.mjs --check")) {
    errors.push("published summary stdout should include check command");
  }

  const outside = run([publishScript, "--out", "../outside.md"]);
  if (outside.status === 0) {
    errors.push("published summary should reject paths outside the repository");
  }
} finally {
  if (tempDir) {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-autonomy-summary: ok");
