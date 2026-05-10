#!/usr/bin/env node
import { spawnSync } from "node:child_process";

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

const markdown = run(["agent-os/scripts/autonomy-summary.mjs", "--limit", "3"]);
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

const json = run(["agent-os/scripts/autonomy-summary.mjs", "--format", "json", "--limit", "5"]);
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
  "agent-os/scripts/autonomy-summary.mjs",
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

const invalidLimit = run(["agent-os/scripts/autonomy-summary.mjs", "--format", "json", "--limit", "0"]);
if (invalidLimit.status === 0) {
  errors.push("autonomy-summary should reject --limit 0");
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-autonomy-summary: ok");
