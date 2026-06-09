#!/usr/bin/env node
import { readFileSync, readdirSync, existsSync, rmSync, cpSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { sh, runStage, testFraction, round } from "./lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..", "..");
process.env.COVENANT_BENCH_ROOT = here; // grade stages run in detached worktrees; git can't find the engine from there
const tasksDir = join(here, "tasks");
const wtRoot = resolve(repoRoot, "..", ".covenant-bench-wt");

const argv = process.argv.slice(2);
const has = (f) => argv.includes(f);
const opt = (f, d = null) => {
  const i = argv.indexOf(f);
  return i >= 0 ? argv[i + 1] : d;
};
const flags = {
  json: has("--json"),
  list: has("--list"),
  keep: has("--keep"),
  solver: opt("--solver", "none"),
  scaffold: opt("--scaffold", resolve(here, "..", "scaffold", "coder.md")),
  scaffoldModel: opt("--scaffold-model", "claude-opus-4-8"),
  only: opt("--task"),
};

function loadTasks() {
  if (!existsSync(tasksDir)) return [];
  return readdirSync(tasksDir)
    .filter((d) => existsSync(join(tasksDir, d, "task.json")))
    .map((d) => ({ dir: join(tasksDir, d), ...JSON.parse(readFileSync(join(tasksDir, d, "task.json"), "utf8")) }))
    .filter((t) => !flags.only || t.id === flags.only)
    .sort((a, b) => a.id.localeCompare(b.id));
}

function addWorktree(base) {
  const path = join(wtRoot, `wt-${base.replace(/[^a-z0-9]/gi, "").slice(0, 12)}-${process.pid}`);
  rmSync(path, { recursive: true, force: true });
  const r = sh("git", ["worktree", "add", "--detach", "--force", path, base], { cwd: repoRoot });
  if (!r.ok) throw new Error(`worktree add failed: ${r.out.trim()}`);
  return path;
}

function dropWorktree(path) {
  sh("git", ["worktree", "remove", "--force", path], { cwd: repoRoot });
  rmSync(path, { recursive: true, force: true });
}

function solve(solver, wt, task) {
  if (solver === "none") return;
  if (solver === "replay") {
    if (!task.solutionCommit) throw new Error("replay solver needs task.solutionCommit");
    const r = sh("git", ["-C", wt, "cherry-pick", "--no-commit", task.solutionCommit]);
    if (!r.ok) throw new Error(`replay failed: ${r.out.trim()}`);
    return;
  }
  if (solver.startsWith("cmd:")) {
    const r = sh("bash", ["-lc", solver.slice(4)], { cwd: wt });
    if (!r.ok) throw new Error(`solver cmd failed: ${r.out.trim()}`);
    return;
  }
  if (solver === "scaffold") {
    const scaffold = readFileSync(flags.scaffold, "utf8");
    const promptFile = join(task.dir, "prompt.md");
    const intent = existsSync(promptFile) ? readFileSync(promptFile, "utf8") : task.prompt ?? task.description;
    const full = `${scaffold}\n\n## Task\n\n${intent}\n\nImplement this in the current working directory. Do not run git and do not commit; leave the file changes in place.`;
    const r = sh("claude", ["-p", full, "--model", flags.scaffoldModel, "--dangerously-skip-permissions"], { cwd: wt, timeout: task.solverTimeoutMs ?? 900000 });
    if (!r.ok) throw new Error(`scaffold solver failed (status ${r.status}): ${r.out.slice(-400)}`);
    return;
  }
  throw new Error(`unknown solver: ${solver}`);
}

// A candidate must not pass by weakening the suite: no removed tests, no added #[ignore].
// Tasks with allowedPaths confine the diff to an allowlist; tasks with evolveFile
// additionally require that everything outside the EVOLVE block is byte-identical.
function gamingViolations(wt, task) {
  const base = task.base;
  const diff = sh("git", ["-C", wt, "diff", base]).out;
  const v = [];
  if (/^\+\s*#\[\s*ignore/m.test(diff)) v.push("added #[ignore]");
  const changed = sh("git", ["-C", wt, "diff", "--name-only", base]).out
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
  if (task.allowedPaths) {
    for (const f of changed) {
      if (!task.allowedPaths.includes(f)) v.push(`out-of-bounds change: ${f}`);
    }
  }
  const changedTests = changed.filter((f) => /(^|\/)tests?\//.test(f) || /_tests?\.rs$/.test(f));
  for (const f of changedTests) {
    const d = sh("git", ["-C", wt, "diff", base, "--", f]).out;
    if (/^-\s*#\[\s*test/m.test(d) || /^-\s*fn\s+\w/m.test(d)) v.push(`weakened tests in ${f}`);
  }
  if (task.evolveFile) {
    const outside = (src) => {
      const a = src.indexOf("// EVOLVE-BLOCK-START");
      const b = src.indexOf("// EVOLVE-BLOCK-END");
      return a >= 0 && b > a ? src.slice(0, a) + src.slice(b) : null;
    };
    const baseSrc = sh("git", ["-C", wt, "show", `${base}:${task.evolveFile}`]).out;
    let cur = null;
    try { cur = readFileSync(join(wt, task.evolveFile), "utf8"); } catch { /* fall through to violation */ }
    const curOutside = cur === null ? null : outside(cur);
    if (curOutside === null) v.push(`EVOLVE markers missing in ${task.evolveFile}`);
    else if (curOutside !== outside(baseSrc)) v.push(`changes outside EVOLVE block in ${task.evolveFile}`);
  }
  return v;
}

function gradeStages(task, cwd) {
  if (existsSync(join(task.dir, "grade"))) {
    cpSync(join(task.dir, "grade"), join(cwd, task.gradeDest ?? "."), { recursive: true });
  }
  const metrics = {};
  let correctness = 1;
  let ms = 0;
  for (const stage of task.graderStages ?? []) {
    const r = runStage(stage, cwd);
    ms += r.ms;
    if (stage.gate && !r.ok) {
      correctness = 0;
      break;
    }
    if (stage.metric === "tests") metrics.tests = round(testFraction(r.out).fraction);
    else if (stage.metric === "clippy") metrics.clippy = r.ok ? 1 : 0;
    else if (stage.metric === "scalar") {
      const m = r.out.match(/^SCALAR ([\d.]+)/m);
      metrics.scalar = r.ok && m ? round(Number(m[1])) : 0;
    } else if (stage.metric) metrics[stage.metric] = r.ok ? 1 : 0;
  }
  return { metrics, correctness, ms };
}

function score(task, metrics, correctness) {
  const w = task.weights ?? {};
  const quality = Object.entries(w).reduce((s, [k, weight]) => s + weight * (metrics[k] ?? 0), 0);
  return round(correctness * quality);
}

function grade(task) {
  if (!task.base) {
    const g = gradeStages(task, repoRoot);
    return { id: task.id, correctness: g.correctness, metrics: g.metrics, score: score(task, g.metrics, g.correctness), ms: g.ms };
  }
  let wt;
  try {
    wt = addWorktree(task.base);
    solve(flags.solver, wt, task);
    const gaming = gamingViolations(wt, task);
    if (gaming.length) return { id: task.id, correctness: 0, score: 0, metrics: {}, gaming, ms: 0 };
    const g = gradeStages(task, wt);
    return { id: task.id, correctness: g.correctness, metrics: g.metrics, score: score(task, g.metrics, g.correctness), ms: g.ms };
  } catch (e) {
    return { id: task.id, correctness: 0, score: 0, metrics: {}, error: String(e.message ?? e), ms: 0 };
  } finally {
    if (wt && !flags.keep) dropWorktree(wt);
  }
}

const tasks = loadTasks();
if (flags.list) {
  console.log(tasks.map((t) => `${t.id}: ${t.description}`).join("\n") || "(no tasks)");
  process.exit(0);
}

const results = tasks.map(grade);
const meanScore = results.length ? round(results.reduce((s, r) => s + r.score, 0) / results.length) : 0;
const report = { schema: "covenant.bench.v2", solver: flags.solver, tasks: results.length, meanScore, results };

if (flags.json) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`# Capability Benchmark (solver=${flags.solver})\n\n- tasks: ${report.tasks}\n- mean score: ${report.meanScore}\n`);
  for (const r of results) {
    const tail = r.error ? ` ERROR ${r.error}` : r.gaming?.length ? ` GAMING ${r.gaming.join("; ")}` : "";
    console.log(`- ${r.id}: ${r.score} (correctness ${r.correctness}, ${JSON.stringify(r.metrics)}, ${Math.round(r.ms / 1000)}s)${tail}`);
  }
}
