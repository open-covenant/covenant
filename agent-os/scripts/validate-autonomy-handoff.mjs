#!/usr/bin/env node
import { spawnSync } from "node:child_process";

function usage() {
  console.log(`usage: validate-autonomy-handoff

Run the read-only autonomy handoff toolchain validation.

Checks dirty report generation, handoff bundle export, bundle verification, restore planning, and expected tamper rejection without writing files or Git metadata.`);
}

function run(args, input = null) {
  const result = spawnSync(process.execPath, args, {
    input,
    encoding: "utf8",
    stdio: ["pipe", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trimEnd(),
    stderr: (result.stderr || "").trimEnd(),
  };
}

function fail(errors) {
  console.error("validate-autonomy-handoff: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

function parseJson(label, text, errors) {
  try {
    return JSON.parse(text);
  } catch (error) {
    errors.push(`${label}: invalid JSON: ${error.message}`);
    return null;
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

const dirtyResult = run(["agent-os/scripts/autonomy-dirty-report.mjs", "--json"]);
if (dirtyResult.status !== 0) {
  errors.push(`autonomy-dirty-report failed: ${dirtyResult.stderr || dirtyResult.stdout}`);
}
const dirtyReport = parseJson("dirty report", dirtyResult.stdout, errors);
if (dirtyReport?.kind !== "autonomy_dirty_report") {
  errors.push("dirty report kind mismatch");
}
if (!Array.isArray(dirtyReport?.dirty_files)) {
  errors.push("dirty report must include dirty_files");
}

const bundleResult = run(["agent-os/scripts/autonomy-handoff-bundle.mjs", "--json"]);
if (bundleResult.status !== 0) {
  errors.push(`autonomy-handoff-bundle failed: ${bundleResult.stderr || bundleResult.stdout}`);
}
const bundle = parseJson("handoff bundle", bundleResult.stdout, errors);
if (bundle?.kind !== "autonomy_handoff_bundle") {
  errors.push("handoff bundle kind mismatch");
}
if (typeof bundle?.tracked_patch !== "string") {
  errors.push("handoff bundle must include tracked_patch");
}
if (!Array.isArray(bundle?.untracked_files)) {
  errors.push("handoff bundle must include untracked_files");
}

const verifyResult = run(
  ["agent-os/scripts/autonomy-verify-handoff-bundle.mjs", "--stdin", "--json"],
  bundleResult.stdout,
);
if (verifyResult.status !== 0) {
  errors.push(`handoff verifier rejected generated bundle: ${verifyResult.stderr || verifyResult.stdout}`);
}
const verification = parseJson("handoff verification", verifyResult.stdout, errors);
if (verification?.valid !== true) {
  errors.push("handoff verification should be valid");
}

const planResult = run(
  ["agent-os/scripts/autonomy-plan-handoff-restore.mjs", "--stdin", "--json"],
  bundleResult.stdout,
);
if (planResult.status !== 0) {
  errors.push(`restore planner rejected generated bundle: ${planResult.stderr || planResult.stdout}`);
}
const plan = parseJson("restore plan", planResult.stdout, errors);
if (plan?.valid !== true) {
  errors.push("restore plan should be valid");
}
if (!Array.isArray(plan?.steps) || plan.steps.length < 5) {
  errors.push("restore plan must include at least five ordered steps");
}

if (bundle) {
  const tampered = {
    ...bundle,
    tracked_patch: `${bundle.tracked_patch || ""}\n# tampered`,
  };
  const tamperedResult = run(
    ["agent-os/scripts/autonomy-verify-handoff-bundle.mjs", "--stdin", "--json"],
    JSON.stringify(tampered),
  );
  if (tamperedResult.status === 0) {
    errors.push("handoff verifier accepted a tampered tracked patch");
  } else {
    const tamperedReport = parseJson("tamper verification", tamperedResult.stdout, errors);
    const tamperDetected = tamperedReport?.errors?.some((error) =>
      error.includes("tracked_patch_sha256"),
    );
    if (!tamperDetected) {
      errors.push("handoff verifier did not report tracked_patch_sha256 tampering");
    }
  }
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-autonomy-handoff: ok");
