#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

function usage() {
  console.log(`usage: autonomy-dirty-report [--json]

Summarize the current dirty working tree and autonomy blockers without mutating Git or task state.`);
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

function parseStatus(text) {
  return text
    .split("\n")
    .filter(Boolean)
    .map((line) => ({
      code: line.slice(0, 2),
      path: line.slice(3),
    }));
}

function loadActiveTasks() {
  const dir = join("agent-os", "autonomy", "tasks");
  return readdirSync(dir)
    .filter((file) => file.endsWith(".json"))
    .map((file) => JSON.parse(readFileSync(join(dir, file), "utf8")))
    .filter((task) => !["integrated", "blocked"].includes(task.state))
    .sort((a, b) => a.id.localeCompare(b.id))
    .map((task) => ({
      id: task.id,
      state: task.state,
      priority: task.priority,
      ownerRole: task.ownerRole,
      nextAction: task.nextAction,
    }));
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

const branch = output(run("git", ["branch", "--show-current"])) || "(detached)";
const head = output(run("git", ["rev-parse", "--short", "HEAD"]));
const status = run("git", ["status", "--porcelain"]);
const diffStat = output(run("git", ["diff", "--stat"]));
const diffCheck = run("git", ["diff", "--check"]);
const preflight = run(process.execPath, ["agent-os/scripts/autonomy-preflight.mjs", "--json"]);

let preflightReport = null;
try {
  preflightReport = JSON.parse(preflight.stdout);
} catch {
  preflightReport = {
    kind: "autonomy_preflight",
    error: output(preflight),
  };
}

const files = status.status === 0 ? parseStatus(status.stdout) : [];
const report = {
  kind: "autonomy_dirty_report",
  generated_at: new Date().toISOString(),
  branch,
  head,
  dirty_count: files.length,
  dirty_files: files,
  diff_stat: diffStat,
  diff_check_ok: diffCheck.status === 0,
  diff_check_output: output(diffCheck),
  active_tasks: loadActiveTasks(),
  preflight: preflightReport,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`autonomy dirty report: ${files.length} dirty path(s)`);
  console.log(`branch: ${branch}`);
  console.log(`head: ${head}`);
  console.log(`diff check: ${report.diff_check_ok ? "ok" : "failed"}`);
  console.log(`commit: ${preflightReport.commit_ready ? "ready" : "blocked"}`);
  console.log(`push: ${preflightReport.push_ready ? "ready" : "blocked"}`);
  if (preflightReport.blockers?.length > 0) {
    console.log(`blockers: ${preflightReport.blockers.join(", ")}`);
  }
  if (files.length > 0) {
    console.log("\ndirty files:");
    for (const file of files) {
      console.log(`  ${file.code} ${file.path}`);
    }
  }
  if (report.active_tasks.length > 0) {
    console.log("\nactive tasks:");
    for (const task of report.active_tasks) {
      console.log(`  ${task.id} (${task.state})`);
    }
  }
  if (diffStat) {
    console.log("\ndiff stat:");
    console.log(diffStat);
  }
}
