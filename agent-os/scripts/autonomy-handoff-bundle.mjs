#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";

const MAX_UNTRACKED_BYTES = 256 * 1024;

function usage() {
  console.log(`usage: autonomy-handoff-bundle [--json]

Emit a read-only handoff bundle for the current dirty tree.

The bundle includes the tracked patch, untracked UTF-8 file contents, dirty report, and restore guidance. It does not stage, commit, push, or write Git metadata.`);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trimEnd(),
    stderr: (result.stderr || "").trimEnd(),
  };
}

function output(result) {
  return [result.stdout, result.stderr].filter(Boolean).join("\n");
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function parseStatus(text) {
  return text
    .split("\n")
    .filter(Boolean)
    .map((line) => ({
      code: line.slice(0, 2),
      path: line.slice(3),
    }));
}

function safePath(path) {
  return !path.startsWith("/") && !path.includes("..") && !path.startsWith(".git/");
}

function untrackedContent(files) {
  return files
    .filter((file) => file.code === "??")
    .map((file) => {
      if (!safePath(file.path)) {
        return { path: file.path, included: false, reason: "unsafe path" };
      }

      let stats;
      try {
        stats = statSync(file.path);
      } catch (error) {
        return { path: file.path, included: false, reason: error.message };
      }

      if (!stats.isFile()) {
        return { path: file.path, included: false, reason: "not a regular file" };
      }

      if (stats.size > MAX_UNTRACKED_BYTES) {
        return { path: file.path, included: false, reason: "file exceeds size cap" };
      }

      const bytes = readFileSync(file.path);
      if (bytes.includes(0)) {
        return { path: file.path, included: false, reason: "binary content" };
      }

      const content = bytes.toString("utf8");
      if (content.includes("\uFFFD")) {
        return { path: file.path, included: false, reason: "non-UTF-8 content" };
      }

      return {
        path: file.path,
        included: true,
        encoding: "utf8",
        bytes: stats.size,
        sha256: sha256(content),
        content,
      };
    });
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
for (const arg of args) {
  if (arg !== "--json") {
    usage();
    process.exit(2);
  }
}

const status = run("git", ["status", "--porcelain"]);
const files = status.status === 0 ? parseStatus(status.stdout) : [];
const patch = output(run("git", ["diff", "--binary"]));
const dirtyReportResult = run(process.execPath, ["agent-os/scripts/autonomy-dirty-report.mjs", "--json"]);

let dirtyReport = null;
try {
  dirtyReport = JSON.parse(dirtyReportResult.stdout);
} catch {
  dirtyReport = {
    kind: "autonomy_dirty_report",
    error: output(dirtyReportResult),
  };
}

const untracked = untrackedContent(files);
const includedUntracked = untracked.filter((file) => file.included);
const skippedUntracked = untracked.filter((file) => !file.included);

const bundle = {
  kind: "autonomy_handoff_bundle",
  generated_at: new Date().toISOString(),
  base_head: output(run("git", ["rev-parse", "HEAD"])),
  branch: output(run("git", ["branch", "--show-current"])) || "(detached)",
  tracked_patch: patch,
  tracked_patch_sha256: sha256(patch),
  dirty_files: files,
  untracked_files: untracked,
  dirty_report: dirtyReport,
  restore: [
    "Apply tracked_patch from repository root with git apply --index only in an environment where Git metadata is writable.",
    "Create included untracked_files at their repository-relative paths before validation.",
    "Run node agent-os/scripts/autonomy-preflight.mjs before committing or pushing.",
  ],
};

if (asJson) {
  console.log(JSON.stringify(bundle, null, 2));
} else {
  console.log("autonomy handoff bundle");
  console.log(`branch: ${bundle.branch}`);
  console.log(`base head: ${bundle.base_head.slice(0, 12)}`);
  console.log(`dirty files: ${files.length}`);
  console.log(`tracked patch bytes: ${Buffer.byteLength(patch)}`);
  console.log(`untracked included: ${includedUntracked.length}`);
  console.log(`untracked skipped: ${skippedUntracked.length}`);
  if (dirtyReport?.preflight?.blockers?.length > 0) {
    console.log(`blockers: ${dirtyReport.preflight.blockers.join(", ")}`);
  }
  console.log("\nrestore:");
  for (const line of bundle.restore) {
    console.log(`  - ${line}`);
  }
}
