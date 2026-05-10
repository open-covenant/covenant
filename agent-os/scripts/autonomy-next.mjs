#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const tasksDir = join(root, "autonomy", "tasks");
const workflow = JSON.parse(readFileSync(join(root, "autonomy", "workflow.json"), "utf8"));

const usage = (stream = process.stdout) => {
  stream.write(`usage: node agent-os/scripts/autonomy-next.mjs [--json] [--seed] [--help]\n\n`);
  stream.write("  --json  Print a structured result (next + candidates + seeded output).\n");
  stream.write("  --seed  Attempt to seed the next task template if the queue is empty.\n");
  stream.write("  --help  Show this help.\n");
};

const rawArgs = process.argv.slice(2);
let asJson = false;
let seed = false;
let help = false;
const unknownArgs = [];

for (const arg of rawArgs) {
  if (arg === "--json") {
    asJson = true;
    continue;
  }
  if (arg === "--seed") {
    seed = true;
    continue;
  }
  if (arg === "--help" || arg === "-h") {
    help = true;
    continue;
  }
  unknownArgs.push(arg);
}

if (unknownArgs.length > 0) {
  process.stderr.write(`autonomy-next: unknown argument(s): ${unknownArgs.join(" ")}\n\n`);
  usage(process.stderr);
  process.exit(2);
}

if (help) {
  usage();
  process.exit(0);
}

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
  if (!seed) {
    console.log("No unblocked autonomous task is ready.");
    console.log("");
    console.log("Try seeding from the backlog:");
    console.log("  node agent-os/scripts/autonomy-next.mjs --seed");
    process.exit(0);
  }
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
