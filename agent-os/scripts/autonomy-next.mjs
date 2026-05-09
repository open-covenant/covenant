#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const tasksDir = join(root, "autonomy", "tasks");
const workflow = JSON.parse(readFileSync(join(root, "autonomy", "workflow.json"), "utf8"));

const args = new Set(process.argv.slice(2));
const asJson = args.has("--json");
const seed = args.has("--seed");

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

let tasks = loadTasks();
let next = tasks[0] ?? null;
let seedOutput = "";

if (!next && seed) {
  const seeded = spawnSync(process.execPath, [join(root, "scripts", "autonomy-seed-next.mjs")], {
    cwd: resolve(root, ".."),
    encoding: "utf8"
  });
  seedOutput = seeded.stdout || seeded.stderr || "";

  if (seeded.status === 0) {
    tasks = loadTasks();
    next = tasks[0] ?? null;
  } else if (seeded.status !== 2) {
    process.stderr.write(seedOutput);
    process.exit(seeded.status ?? 1);
  }
}

if (asJson) {
  console.log(JSON.stringify({ next, candidates: tasks, seeded: seedOutput.trim() || null }, null, 2));
  process.exit(0);
}

if (seedOutput) {
  process.stdout.write(seedOutput);
}

if (!next) {
  console.log("No unblocked autonomous task is ready.");
  process.exit(0);
}

console.log(`${next.id}: ${next.title}`);
console.log(`state: ${next.state}`);
console.log(`priority: ${next.priority}`);
console.log(`owner role: ${next.ownerRole}`);
console.log("");
console.log(next.summary);
console.log("");
console.log("next action:");
console.log(`  ${next.nextAction}`);
console.log("");
console.log("gates:");
for (const gate of next.gates) {
  console.log(`  - ${gate}`);
}
console.log("");
console.log("verification:");
for (const command of next.verification) {
  console.log(`  - ${command}`);
}
