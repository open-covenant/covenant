#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

function usage() {
  console.log(`usage: autonomy-verify-handoff-bundle (--stdin | <bundle.json>) [--json]

Verify an autonomy handoff bundle without restoring files or writing Git metadata.`);
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

function scanForbidden(value, errors, pointer = "$") {
  const forbidden = [
    [new RegExp(`/${"Users"}/[^\\s"')\\]]+`), "machine-local home path"],
    [new RegExp(`Co-${"Authored-By"}:`, "i"), "commit attribution trailer"],
    [new RegExp(`${"Generated"} with`, "i"), "AI generation attribution"],
  ];

  if (typeof value === "string") {
    for (const [pattern, label] of forbidden) {
      if (pattern.test(value)) {
        errors.push(`${pointer} contains forbidden ${label}`);
      }
    }
    return;
  }

  if (Array.isArray(value)) {
    value.forEach((item, index) => scanForbidden(item, errors, `${pointer}[${index}]`));
    return;
  }

  if (value && typeof value === "object") {
    for (const [key, item] of Object.entries(value)) {
      scanForbidden(item, errors, `${pointer}.${key}`);
    }
  }
}

function validateFile(file, index, errors) {
  const prefix = `untracked_files[${index}]`;
  if (!safePath(file?.path)) {
    errors.push(`${prefix}.path must be a safe repository-relative path`);
  }
  if (typeof file?.included !== "boolean") {
    errors.push(`${prefix}.included must be boolean`);
    return;
  }
  if (!file.included) {
    if (typeof file.reason !== "string" || file.reason.trim() === "") {
      errors.push(`${prefix}.reason must explain skipped files`);
    }
    return;
  }

  if (file.encoding !== "utf8") {
    errors.push(`${prefix}.encoding must be utf8`);
  }
  if (typeof file.content !== "string") {
    errors.push(`${prefix}.content must be a string`);
    return;
  }
  if (!Number.isInteger(file.bytes) || file.bytes !== Buffer.byteLength(file.content)) {
    errors.push(`${prefix}.bytes must match UTF-8 content length`);
  }
  if (file.sha256 !== sha256(file.content)) {
    errors.push(`${prefix}.sha256 does not match content`);
  }
}

function validateBundle(bundle) {
  const errors = [];

  if (bundle?.kind !== "autonomy_handoff_bundle") {
    errors.push("kind must be autonomy_handoff_bundle");
  }
  if (!/^\d{4}-\d{2}-\d{2}T/.test(bundle?.generated_at || "")) {
    errors.push("generated_at must be ISO-like");
  }
  if (!/^[0-9a-f]{40}$/.test(bundle?.base_head || "")) {
    errors.push("base_head must be a 40-character lowercase hex SHA");
  }
  if (typeof bundle?.branch !== "string" || bundle.branch.trim() === "") {
    errors.push("branch must be present");
  }
  if (typeof bundle?.tracked_patch !== "string") {
    errors.push("tracked_patch must be a string");
  } else if (bundle.tracked_patch_sha256 !== sha256(bundle.tracked_patch)) {
    errors.push("tracked_patch_sha256 does not match tracked_patch");
  }

  if (!Array.isArray(bundle?.dirty_files)) {
    errors.push("dirty_files must be an array");
  } else {
    bundle.dirty_files.forEach((file, index) => {
      if (typeof file?.code !== "string" || file.code.length !== 2) {
        errors.push(`dirty_files[${index}].code must be a two-character porcelain code`);
      }
      if (!safePath(file?.path)) {
        errors.push(`dirty_files[${index}].path must be a safe repository-relative path`);
      }
    });
  }

  if (!Array.isArray(bundle?.untracked_files)) {
    errors.push("untracked_files must be an array");
  } else {
    bundle.untracked_files.forEach((file, index) => validateFile(file, index, errors));
  }

  if (bundle?.dirty_report?.kind !== "autonomy_dirty_report") {
    errors.push("dirty_report.kind must be autonomy_dirty_report");
  }
  if (!Array.isArray(bundle?.restore) || bundle.restore.length === 0) {
    errors.push("restore must be a non-empty array");
  }

  scanForbidden(bundle, errors);
  return errors;
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
  const input = fromStdin ? readFileSync(0, "utf8") : readFileSync(file, "utf8");
  bundle = JSON.parse(input);
} catch (error) {
  console.error("autonomy-verify-handoff-bundle: failed");
  console.error(`- cannot read bundle: ${error.message}`);
  process.exit(1);
}

const errors = validateBundle(bundle);
const report = {
  kind: "autonomy_handoff_bundle_verification",
  valid: errors.length === 0,
  errors,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else if (errors.length === 0) {
  console.log("autonomy-verify-handoff-bundle: ok");
} else {
  console.error("autonomy-verify-handoff-bundle: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
}

if (errors.length > 0) {
  process.exit(1);
}
