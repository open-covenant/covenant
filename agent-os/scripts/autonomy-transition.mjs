#!/usr/bin/env node
import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const workflowPath = join(root, "autonomy", "workflow.json");
const tasksDir = join(root, "autonomy", "tasks");
const eventsPath = join(root, "autonomy", "events.jsonl");

const usage = () => {
  console.error("usage: scripts/autonomy-transition.mjs <task-id> <state> [--actor role] [--note text]");
};

const [taskId, targetState, ...rest] = process.argv.slice(2);
if (!taskId || !targetState) {
  usage();
  process.exit(2);
}

let actorRole = "implementer";
let note = "";
for (let index = 0; index < rest.length; index += 1) {
  const arg = rest[index];
  if (arg === "--actor") {
    actorRole = rest[index + 1] ?? "";
    index += 1;
    continue;
  }
  if (arg === "--note") {
    note = rest[index + 1] ?? "";
    index += 1;
    continue;
  }
  usage();
  process.exit(2);
}

const workflow = JSON.parse(readFileSync(workflowPath, "utf8"));
const validStates = new Set(workflow.states);
const validRoles = new Set(workflow.roles);
const transitions = new Set(workflow.transitions.map(([from, to]) => `${from}->${to}`));

if (!validStates.has(targetState)) {
  console.error(`unknown target state: ${targetState}`);
  process.exit(2);
}

if (!validRoles.has(actorRole)) {
  console.error(`unknown actor role: ${actorRole}`);
  process.exit(2);
}

const taskPath = join(tasksDir, `${taskId}.json`);
let previousTaskText;
let task;
try {
  previousTaskText = readFileSync(taskPath, "utf8");
  task = JSON.parse(previousTaskText);
} catch {
  console.error(`unknown task: ${taskId}`);
  process.exit(1);
}

const from = task.state;
if (from !== targetState && !transitions.has(`${from}->${targetState}`)) {
  console.error(`invalid transition: ${from} -> ${targetState}`);
  process.exit(1);
}

const now = new Date();
const previousEventsText = existsSync(eventsPath) ? readFileSync(eventsPath, "utf8") : null;

task.state = targetState;
task.updated = now.toISOString().slice(0, 10);

writeFileSync(taskPath, `${JSON.stringify(task, null, 2)}\n`);

const event = {
  timestamp: now.toISOString(),
  taskId,
  from,
  to: targetState,
  actorRole,
  note
};
appendFileSync(eventsPath, `${JSON.stringify(event)}\n`);

const validation = spawnSync(process.execPath, [join(root, "scripts", "validate-autonomy.mjs")], {
  cwd: resolve(root, ".."),
  encoding: "utf8"
});

if (validation.status !== 0) {
  writeFileSync(taskPath, previousTaskText);
  if (previousEventsText === null) {
    writeFileSync(eventsPath, "");
  } else {
    writeFileSync(eventsPath, previousEventsText);
  }
  process.stderr.write(validation.stderr || validation.stdout);
  process.exit(validation.status ?? 1);
}

console.log(`${taskId}: ${from} -> ${targetState}`);
