#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const tasksDir = join(root, "autonomy", "tasks");
const workflow = JSON.parse(readFileSync(join(root, "autonomy", "workflow.json"), "utf8"));

const validation = spawnSync(process.execPath, [join(root, "scripts", "validate-autonomy.mjs")], {
  cwd: resolve(root, ".."),
  encoding: "utf8"
});

if (validation.status !== 0) {
  process.stderr.write(validation.stderr || validation.stdout);
  process.exit(validation.status ?? 1);
}

const priorityRank = new Map(workflow.priorities.map((priority, index) => [
  priority,
  index + 1
]));

const stateRank = new Map([
  ["repair", 100],
  ["validation", 90],
  ["cross_review", 80],
  ["self_review", 70],
  ["in_progress", 60],
  ["planned", 50],
  ["triaged", 40],
  ["proposed", 30]
]);

const loadTasks = () =>
  readdirSync(tasksDir)
    .filter((file) => file.endsWith(".json"))
    .map((file) => JSON.parse(readFileSync(join(tasksDir, file), "utf8")))
    .filter((task) => !["integrated", "blocked"].includes(task.state))
    .sort((a, b) => {
      const byState = (stateRank.get(b.state) ?? 0) - (stateRank.get(a.state) ?? 0);
      if (byState !== 0) return byState;
      const byPriority = (priorityRank.get(b.priority) ?? 0) - (priorityRank.get(a.priority) ?? 0);
      if (byPriority !== 0) return byPriority;
      return a.id.localeCompare(b.id);
    });

let next = loadTasks()[0] ?? null;

if (!next) {
  const seeded = spawnSync(process.execPath, [join(root, "scripts", "autonomy-seed-next.mjs")], {
    cwd: resolve(root, ".."),
    encoding: "utf8"
  });
  process.stdout.write(seeded.stdout || "");
  if (seeded.status !== 0) {
    process.stderr.write(seeded.stderr || "");
    console.log("continuation: no unblocked autonomous task is ready");
    process.exit(seeded.status ?? 2);
  }
  next = loadTasks()[0] ?? null;
  if (!next) {
    console.log("continuation: seeded backlog task, but no unblocked autonomous task is ready");
    process.exit(2);
  }
}

console.log(`continuation: continue with ${next.id}`);
console.log(`state: ${next.state}`);
console.log(`priority: ${next.priority}`);
console.log(`next action: ${next.nextAction}`);
console.log("");
console.log("rule: do not stop at this point unless every candidate is blocked, the user explicitly asks to pause, or an external system ends the turn.");
