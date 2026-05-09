#!/usr/bin/env node
import { appendFileSync, existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const backlogPath = join(root, "autonomy", "backlog.json");
const tasksDir = join(root, "autonomy", "tasks");
const workflowPath = join(root, "autonomy", "workflow.json");

const fail = (message, code = 1) => {
  console.error(message);
  process.exit(code);
};

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const workflow = readJson(workflowPath);
const roles = new Set(workflow.roles || []);

let dryRun = false;
let actorRole = "planner";
let note = "Seeded from backlog template.";
const args = process.argv.slice(2);

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--dry-run") {
    dryRun = true;
    continue;
  }
  if (arg === "--actor") {
    actorRole = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--note") {
    note = args[index + 1] ?? "";
    index += 1;
    continue;
  }
  fail("usage: autonomy-seed-next.mjs [--dry-run] [--actor role] [--note text]", 2);
}

if (!roles.has(actorRole)) {
  fail(`autonomy-seed-next: unknown actor role: ${actorRole}`, 2);
}

if (note.trim() === "") {
  fail("autonomy-seed-next: --note must be non-empty", 2);
}

let backlog;
try {
  backlog = readJson(backlogPath);
} catch (error) {
  fail(`autonomy-seed-next: cannot read backlog: ${error.message}`);
}

if (!Array.isArray(backlog.tasks) || backlog.tasks.length === 0) {
  fail("autonomy-seed-next: backlog has no task templates", 2);
}

const next = backlog.tasks.find((task) => {
  return task?.id && !existsSync(join(tasksDir, `${task.id}.json`));
});

if (!next) {
  fail("autonomy-seed-next: backlog exhausted", 2);
}

const task = {
  ...next,
  state: "proposed",
  updated: new Date().toISOString().slice(0, 10)
};

const taskPath = join(tasksDir, `${task.id}.json`);
const eventsPath = join(root, "autonomy", "events.jsonl");

if (dryRun) {
  console.log(`autonomy-seed-next: would seed ${task.id}: ${task.title}`);
  process.exit(0);
}

const validate = () => spawnSync(process.execPath, [join(root, "scripts", "validate-autonomy.mjs")], {
  cwd: resolve(root, ".."),
  encoding: "utf8"
});

writeFileSync(taskPath, `${JSON.stringify(task, null, 2)}\n`);

const validation = validate();
if (validation.status !== 0) {
  try {
    unlinkSync(taskPath);
  } catch {
    // Best-effort rollback. The validation output below names the broken task.
  }
  process.stderr.write(validation.stderr || validation.stdout);
  process.exit(validation.status ?? 1);
}

const previousEventsText = existsSync(eventsPath) ? readFileSync(eventsPath, "utf8") : null;
const event = {
  timestamp: new Date().toISOString(),
  taskId: task.id,
  from: null,
  to: "proposed",
  actorRole,
  note
};
appendFileSync(eventsPath, `${JSON.stringify(event)}\n`);

const eventValidation = validate();
if (eventValidation.status !== 0) {
  try {
    unlinkSync(taskPath);
  } catch {
    // Best-effort rollback. The validation output below names the broken task.
  }
  if (previousEventsText === null) {
    writeFileSync(eventsPath, "");
  } else {
    writeFileSync(eventsPath, previousEventsText);
  }
  process.stderr.write(eventValidation.stderr || eventValidation.stdout);
  process.exit(eventValidation.status ?? 1);
}

console.log(`autonomy-seed-next: seeded ${task.id}: ${task.title}`);
