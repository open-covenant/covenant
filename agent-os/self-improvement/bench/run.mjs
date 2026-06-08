#!/usr/bin/env node
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runStage, testFraction } from "./lib/grade.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..", "..");
const tasksDir = join(here, "tasks");

const argv = process.argv.slice(2);
const flags = { json: argv.includes("--json"), list: argv.includes("--list") };
const only = (() => {
  const i = argv.indexOf("--task");
  return i >= 0 ? argv[i + 1] : null;
})();

const round = (n) => Math.round(n * 1000) / 1000;

function loadTasks() {
  if (!existsSync(tasksDir)) return [];
  return readdirSync(tasksDir)
    .filter((d) => existsSync(join(tasksDir, d, "task.json")))
    .map((d) => JSON.parse(readFileSync(join(tasksDir, d, "task.json"), "utf8")))
    .filter((t) => !only || t.id === only)
    .sort((a, b) => a.id.localeCompare(b.id));
}

function gradeTask(task) {
  const metrics = {};
  let correctness = 1;
  let ms = 0;
  for (const stage of task.stages) {
    const r = runStage(stage, repoRoot);
    ms += r.ms;
    if (stage.gate && !r.ok) {
      correctness = 0;
      break;
    }
    if (stage.metric === "tests") metrics.tests = round(testFraction(r.out).fraction);
    if (stage.metric === "clippy") metrics.clippy = r.ok ? 1 : 0;
  }
  const weights = task.weights ?? {};
  const quality = Object.entries(weights).reduce((s, [k, w]) => s + w * (metrics[k] ?? 0), 0);
  return { id: task.id, correctness, metrics, score: round(correctness * quality), ms };
}

const tasks = loadTasks();

if (flags.list) {
  console.log(tasks.map((t) => `${t.id}: ${t.description}`).join("\n") || "(no tasks)");
  process.exit(0);
}

const results = tasks.map(gradeTask);
const meanScore = results.length ? round(results.reduce((s, r) => s + r.score, 0) / results.length) : 0;
const report = { schema: "covenant.bench.v1", tasks: results.length, meanScore, results };

if (flags.json) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`# Capability Benchmark\n\n- tasks: ${report.tasks}\n- mean score: ${report.meanScore}\n`);
  for (const r of results) {
    console.log(`- ${r.id}: ${r.score} (correctness ${r.correctness}, ${JSON.stringify(r.metrics)}, ${Math.round(r.ms / 1000)}s)`);
  }
}
