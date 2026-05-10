#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

function usage() {
  console.log(`usage: autonomy-plan-handoff-restore (--stdin | <bundle.json>) [--json]

Plan restoration of an autonomy handoff bundle without writing files or Git metadata.`);
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function safePath(path) {
  return typeof path === "string"
    && path.length > 0
    && !path.startsWith("/")
    && !path.includes("..")
    && !path.startsWith(".git/");
}

function readBundle({ fromStdin, file }) {
  const input = fromStdin ? readFileSync(0, "utf8") : readFileSync(file, "utf8");
  return JSON.parse(input);
}

function validateBundle(bundle) {
  const errors = [];
  if (bundle?.kind !== "autonomy_handoff_bundle") {
    errors.push("kind must be autonomy_handoff_bundle");
  }
  if (!/^[0-9a-f]{40}$/.test(bundle?.base_head || "")) {
    errors.push("base_head must be a 40-character lowercase hex SHA");
  }
  if (typeof bundle?.tracked_patch !== "string") {
    errors.push("tracked_patch must be a string");
  } else if (bundle.tracked_patch_sha256 !== sha256(bundle.tracked_patch)) {
    errors.push("tracked_patch_sha256 does not match tracked_patch");
  }
  if (!Array.isArray(bundle?.untracked_files)) {
    errors.push("untracked_files must be an array");
  } else {
    for (const [index, file] of bundle.untracked_files.entries()) {
      if (!safePath(file?.path)) {
        errors.push(`untracked_files[${index}].path is not safe`);
      }
      if (file?.included) {
        if (typeof file.content !== "string") {
          errors.push(`untracked_files[${index}].content must be a string`);
        } else {
          if (file.sha256 !== sha256(file.content)) {
            errors.push(`untracked_files[${index}].sha256 does not match content`);
          }
          if (file.bytes !== Buffer.byteLength(file.content)) {
            errors.push(`untracked_files[${index}].bytes does not match content`);
          }
        }
      }
    }
  }
  if (bundle?.dirty_report?.kind !== "autonomy_dirty_report") {
    errors.push("dirty_report.kind must be autonomy_dirty_report");
  }
  return errors;
}

function plan(bundle) {
  const included = bundle.untracked_files.filter((file) => file.included);
  const skipped = bundle.untracked_files.filter((file) => !file.included);
  const blockers = bundle.dirty_report?.preflight?.blockers || [];

  return {
    kind: "autonomy_handoff_restore_plan",
    generated_at: new Date().toISOString(),
    base_head: bundle.base_head,
    branch: bundle.branch,
    dirty_count: bundle.dirty_files?.length || 0,
    tracked_patch_sha256: bundle.tracked_patch_sha256,
    untracked_included: included.map((file) => ({
      path: file.path,
      bytes: file.bytes,
      sha256: file.sha256,
    })),
    untracked_skipped: skipped.map((file) => ({
      path: file.path,
      reason: file.reason,
    })),
    blockers,
    steps: [
      {
        order: 1,
        action: "checkout_base",
        detail: `Start from commit ${bundle.base_head}. Do not restore onto a different base without reviewing the patch.`,
      },
      {
        order: 2,
        action: "write_untracked_files",
        detail: `Create ${included.length} included untracked text file(s) at their repository-relative paths before applying the tracked patch.`,
      },
      {
        order: 3,
        action: "apply_tracked_patch",
        detail: "Apply tracked_patch from repository root, then inspect the resulting diff before staging.",
      },
      {
        order: 4,
        action: "run_preflight",
        detail: "Run node agent-os/scripts/autonomy-preflight.mjs and resolve commit or push blockers before committing or pushing.",
      },
      {
        order: 5,
        action: "validate",
        detail: "Run node agent-os/scripts/validate-autonomy.mjs, git diff --check, and any task-specific checks before commit.",
      },
    ],
  };
}

const argv = process.argv.slice(2);
if (argv.includes("--help") || argv.includes("-h")) {
  usage();
  process.exit(0);
}

let asJson = false;
let fromStdin = false;
let file = "";
for (const arg of argv) {
  if (arg === "--json") {
    asJson = true;
    continue;
  }
  if (arg === "--stdin") {
    fromStdin = true;
    continue;
  }
  if (!file) {
    file = arg;
    continue;
  }
  usage();
  process.exit(2);
}

if ((fromStdin && file) || (!fromStdin && !file)) {
  usage();
  process.exit(2);
}

let bundle;
try {
  bundle = readBundle({ fromStdin, file });
} catch (error) {
  console.error("autonomy-plan-handoff-restore: failed");
  console.error(`- cannot read bundle: ${error.message}`);
  process.exit(1);
}

const errors = validateBundle(bundle);
if (errors.length > 0) {
  if (asJson) {
    console.log(JSON.stringify({
      kind: "autonomy_handoff_restore_plan",
      valid: false,
      errors,
    }, null, 2));
  } else {
    console.error("autonomy-plan-handoff-restore: failed");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
  }
  process.exit(1);
}

const restorePlan = plan(bundle);
if (asJson) {
  console.log(JSON.stringify({
    valid: true,
    ...restorePlan,
  }, null, 2));
} else {
  console.log("autonomy handoff restore plan");
  console.log(`base: ${restorePlan.base_head}`);
  console.log(`branch: ${restorePlan.branch}`);
  console.log(`dirty files: ${restorePlan.dirty_count}`);
  console.log(`untracked included: ${restorePlan.untracked_included.length}`);
  console.log(`untracked skipped: ${restorePlan.untracked_skipped.length}`);
  if (restorePlan.blockers.length > 0) {
    console.log(`source blockers: ${restorePlan.blockers.join(", ")}`);
  }
  console.log("\nsteps:");
  for (const step of restorePlan.steps) {
    console.log(`  ${step.order}. ${step.action}: ${step.detail}`);
  }
}
