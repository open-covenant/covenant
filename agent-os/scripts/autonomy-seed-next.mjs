#!/usr/bin/env node
import { existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const backlogPath = join(root, "autonomy", "backlog.json");
const tasksDir = join(root, "autonomy", "tasks");

const args = new Set(process.argv.slice(2));
const dryRun = args.has("--dry-run");

const fail = (message, code = 1) => {
  console.error(message);
  process.exit(code);
};

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

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

if (dryRun) {
  console.log(`autonomy-seed-next: would seed ${task.id}: ${task.title}`);
  process.exit(0);
}

writeFileSync(taskPath, `${JSON.stringify(task, null, 2)}\n`);

const validation = spawnSync(process.execPath, [join(root, "scripts", "validate-autonomy.mjs")], {
  cwd: resolve(root, ".."),
  encoding: "utf8"
});

if (validation.status !== 0) {
  try {
    unlinkSync(taskPath);
  } catch {
    // Best-effort rollback. The validation output below names the broken task.
  }
  process.stderr.write(validation.stderr || validation.stdout);
  process.exit(validation.status ?? 1);
}

console.log(`autonomy-seed-next: seeded ${task.id}: ${task.title}`);
