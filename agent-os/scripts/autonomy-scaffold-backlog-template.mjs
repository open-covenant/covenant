#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const backlogPath = join(root, "autonomy", "backlog.json");
const tasksDir = join(root, "autonomy", "tasks");
const workflowPath = join(root, "autonomy", "workflow.json");

const usage = () => {
  console.error(`usage: autonomy-scaffold-backlog-template.mjs <id> \\
  --title text \\
  --summary text \\
  --next-action text \\
  --failure text --failure text --failure text \\
  [--priority high] [--owner-role implementer] \\
  [--scope path] [--gate docs] [--verification command] \\
  [--human-escalation text] [--dry-run]`);
};

const fail = (message, code = 1) => {
  console.error(message);
  process.exit(code);
};

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const requireText = (value, name) => {
  if (typeof value !== "string" || value.trim() === "") {
    fail(`autonomy-scaffold-backlog-template: ${name} is required`, 2);
  }
  return value.trim();
};

const pushOption = (target, value, name) => {
  target.push(requireText(value, name));
};

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}

let id = "";
let title = "";
let summary = "";
let nextAction = "";
let priority = "high";
let ownerRole = "implementer";
let dryRun = false;
const scope = [];
const gates = [];
const verification = [];
const expectedFailureModes = [];
const humanEscalation = [];

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (!arg.startsWith("--") && !id) {
    id = arg;
    continue;
  }
  if (arg === "--dry-run") {
    dryRun = true;
    continue;
  }
  if (arg === "--title") {
    title = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--summary") {
    summary = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--next-action") {
    nextAction = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--priority") {
    priority = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--owner-role") {
    ownerRole = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--scope") {
    pushOption(scope, args[index + 1] ?? "", "--scope");
    index += 1;
    continue;
  }
  if (arg === "--gate") {
    pushOption(gates, args[index + 1] ?? "", "--gate");
    index += 1;
    continue;
  }
  if (arg === "--verification") {
    pushOption(verification, args[index + 1] ?? "", "--verification");
    index += 1;
    continue;
  }
  if (arg === "--failure") {
    pushOption(expectedFailureModes, args[index + 1] ?? "", "--failure");
    index += 1;
    continue;
  }
  if (arg === "--human-escalation") {
    pushOption(humanEscalation, args[index + 1] ?? "", "--human-escalation");
    index += 1;
    continue;
  }
  usage();
  fail(`autonomy-scaffold-backlog-template: unknown argument: ${arg}`, 2);
}

id = requireText(id, "id");
title = requireText(title, "--title");
summary = requireText(summary, "--summary");
nextAction = requireText(nextAction, "--next-action");
priority = requireText(priority, "--priority");
ownerRole = requireText(ownerRole, "--owner-role");

if (!/^[a-z0-9][a-z0-9-]*$/.test(id)) {
  fail("autonomy-scaffold-backlog-template: id must be lowercase kebab-case", 2);
}

if (expectedFailureModes.length < 3) {
  fail("autonomy-scaffold-backlog-template: provide at least three --failure values", 2);
}

const workflow = readJson(workflowPath);
const priorities = new Set(workflow.priorities || []);
const roles = new Set(workflow.roles || []);
const gateNames = new Set(Object.keys(workflow.gates || {}));

if (!priorities.has(priority)) {
  fail(`autonomy-scaffold-backlog-template: priority must be one of: ${[...priorities].join(", ")}`, 2);
}

if (!roles.has(ownerRole)) {
  fail(`autonomy-scaffold-backlog-template: owner-role must be one of: ${[...roles].join(", ")}`, 2);
}

const finalScope = scope.length > 0 ? scope : ["docs"];
const finalGates = gates.length > 0 ? gates : ["docs"];
const finalVerification = verification.length > 0
  ? verification
  : ["node agent-os/scripts/validate-autonomy.mjs", "git diff --check"];

for (const entry of finalScope) {
  if (entry.startsWith("/") || entry.includes("..")) {
    fail(`autonomy-scaffold-backlog-template: scope must be a relative repository path: ${entry}`, 2);
  }
}

for (const gate of finalGates) {
  if (!gateNames.has(gate)) {
    fail(`autonomy-scaffold-backlog-template: gate must be one of: ${[...gateNames].join(", ")}`, 2);
  }
}

const backlog = readJson(backlogPath);
if (!Array.isArray(backlog.tasks)) {
  fail("autonomy-scaffold-backlog-template: backlog.tasks must be an array");
}

if (backlog.tasks.some((task) => task?.id === id)) {
  fail(`autonomy-scaffold-backlog-template: backlog already contains template id: ${id}`, 2);
}

if (existsSync(join(tasksDir, `${id}.json`))) {
  fail(`autonomy-scaffold-backlog-template: task already exists: agent-os/autonomy/tasks/${id}.json`, 2);
}

const task = {
  id,
  title,
  state: "proposed",
  priority,
  ownerRole,
  scope: finalScope,
  summary,
  expectedFailureModes,
  gates: finalGates,
  verification: finalVerification,
  humanEscalation,
  nextAction,
  updated: new Date().toISOString().slice(0, 10)
};

if (dryRun) {
  console.log(JSON.stringify(task, null, 2));
  process.exit(0);
}

const previousBacklog = readFileSync(backlogPath, "utf8");
backlog.tasks.push(task);
writeFileSync(backlogPath, `${JSON.stringify(backlog, null, 2)}\n`);

const validation = spawnSync(process.execPath, [join(root, "scripts", "validate-autonomy.mjs")], {
  cwd: resolve(root, ".."),
  encoding: "utf8"
});

if (validation.status !== 0) {
  writeFileSync(backlogPath, previousBacklog);
  process.stderr.write(validation.stderr || validation.stdout);
  process.exit(validation.status ?? 1);
}

console.log(`autonomy-scaffold-backlog-template: appended ${id}`);
console.log("checklist before seeding:");
console.log("  - confirm scope covers every file family the task will touch");
console.log("  - confirm gates match the risk surface");
console.log("  - confirm verification commands are deterministic and machine-portable");
console.log("  - confirm expected failure modes describe real regressions");
