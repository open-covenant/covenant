#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const tasksDir = join(root, "autonomy", "tasks");
const eventsPath = join(root, "autonomy", "events.jsonl");

const round = (n) => (n == null ? null : Math.round(n * 10) / 10);
const ratio = (a, b) => (b > 0 ? Math.round((a / b) * 100) / 100 : null);
const taskType = (id) => /^([a-z0-9]+(?:-[a-z]+)?)/i.exec(id || "")?.[1] ?? "unknown";

function quantile(sorted, q) {
  if (sorted.length === 0) return null;
  const pos = (sorted.length - 1) * q;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  return lo === hi ? sorted[lo] : sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}

function usage() {
  console.error("usage: node agent-os/scripts/autonomy-metrics.mjs [--since YYYY-MM-DD] [--format markdown|json] [--json] [--top N]");
}

function parseFlags(args) {
  const flags = { since: null, format: "markdown", top: 10 };
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === "--json") { flags.format = "json"; continue; }
    const value = args[i + 1];
    if (!value || value.startsWith("--")) { usage(); process.exit(2); }
    i += 1;
    if (arg === "--since") {
      if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) throw new Error("--since must use YYYY-MM-DD");
      flags.since = value;
    } else if (arg === "--format") {
      if (!["markdown", "json"].includes(value)) throw new Error("--format must be markdown or json");
      flags.format = value;
    } else if (arg === "--top") {
      const n = Number.parseInt(value, 10);
      if (!Number.isSafeInteger(n) || n < 1 || n > 100) throw new Error("--top must be 1..100");
      flags.top = n;
    } else { usage(); process.exit(2); }
  }
  return flags;
}

function readEvents() {
  return readFileSync(eventsPath, "utf8")
    .split(/\r?\n/)
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l));
}

function readTasks() {
  try {
    return readdirSync(tasksDir)
      .filter((n) => n.endsWith(".json"))
      .map((n) => JSON.parse(readFileSync(join(tasksDir, n), "utf8")));
  } catch {
    return [];
  }
}

function buildMetrics(flags) {
  const all = readEvents();
  const cutoff = flags.since ? `${flags.since}T00:00:00.000Z` : null;
  const events = cutoff ? all.filter((e) => String(e.timestamp) >= cutoff) : all;

  const byTask = new Map();
  for (const e of events) {
    if (!byTask.has(e.taskId)) byTask.set(e.taskId, []);
    byTask.get(e.taskId).push(e);
  }
  for (const list of byTask.values()) {
    list.sort((a, b) => String(a.timestamp).localeCompare(String(b.timestamp)));
  }

  const reachedValidation = new Set();
  const enteredRepair = new Set();
  const repairByTask = new Map();
  const durations = {};
  const roleCount = {};
  let validationPass = 0;
  let validationFail = 0;

  for (const [taskId, list] of byTask) {
    for (let i = 0; i < list.length; i += 1) {
      const e = list[i];
      roleCount[e.actorRole ?? "unknown"] = (roleCount[e.actorRole ?? "unknown"] ?? 0) + 1;
      if (e.to === "validation") reachedValidation.add(taskId);
      if (e.to === "repair") {
        enteredRepair.add(taskId);
        repairByTask.set(taskId, (repairByTask.get(taskId) ?? 0) + 1);
      }
      if (e.from === "validation" && e.to === "ready") validationPass += 1;
      if (e.from === "validation" && e.to === "repair") validationFail += 1;
      const next = list[i + 1];
      if (next && e.to) {
        const mins = (Date.parse(next.timestamp) - Date.parse(e.timestamp)) / 60000;
        if (Number.isFinite(mins) && mins >= 0) (durations[e.to] ??= []).push(mins);
      }
    }
  }

  const timeInState = {};
  for (const [state, arr] of Object.entries(durations)) {
    arr.sort((a, b) => a - b);
    timeInState[state] = {
      count: arr.length,
      medianMinutes: round(quantile(arr, 0.5)),
      p90Minutes: round(quantile(arr, 0.9)),
    };
  }

  const repairByType = {};
  for (const taskId of enteredRepair) {
    repairByType[taskType(taskId)] = (repairByType[taskType(taskId)] ?? 0) + 1;
  }

  const activeDays = new Set(events.map((e) => String(e.timestamp).slice(0, 10))).size || 1;
  const integrated = events.filter((e) => e.to === "integrated").length;

  const worstTasks = [...repairByTask.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, flags.top)
    .map(([id, repairs]) => ({ id, repairs, type: taskType(id) }));

  return {
    schema: "covenant.autonomy-metrics.v1",
    generatedAt: events.at(-1)?.timestamp ?? null,
    since: flags.since,
    window: { events: events.length, tasksTouched: byTask.size, activeDays },
    repair: {
      tasksReachedValidation: reachedValidation.size,
      tasksEnteredRepair: enteredRepair.size,
      repairRate: ratio(enteredRepair.size, reachedValidation.size),
      byType: repairByType,
    },
    validation: {
      passes: validationPass,
      failures: validationFail,
      failureRate: ratio(validationFail, validationPass + validationFail),
    },
    throughput: {
      integrated,
      integratedPerDay: round(integrated / activeDays),
      transitionsPerDay: round(events.length / activeDays),
    },
    timeInState,
    byActorRole: roleCount,
    worstTasks,
  };
}

function renderMarkdown(m) {
  const tis = Object.entries(m.timeInState)
    .map(([s, d]) => `  - ${s}: median ${d.medianMinutes}m, p90 ${d.p90Minutes}m (n=${d.count})`)
    .join("\n");
  const worst = m.worstTasks.length
    ? m.worstTasks.map((t) => `  - ${t.id} (${t.type}): ${t.repairs} repair(s)`).join("\n")
    : "  - none";
  const roles = Object.entries(m.byActorRole).map(([r, c]) => `${r}:${c}`).join(", ");
  return `# Autonomy Metrics${m.since ? ` since ${m.since}` : ""}

- generated: ${m.generatedAt}
- window: ${m.window.events} events, ${m.window.tasksTouched} tasks, ${m.window.activeDays} active day(s)

## Repair / rework
- tasks reached validation: ${m.repair.tasksReachedValidation}
- tasks entered repair: ${m.repair.tasksEnteredRepair}
- repair rate: ${m.repair.repairRate}
- validation failure rate: ${m.validation.failureRate} (${m.validation.failures} fail / ${m.validation.passes} pass)

## Throughput
- integrated: ${m.throughput.integrated}
- integrated / day: ${m.throughput.integratedPerDay}
- transitions / day: ${m.throughput.transitionsPerDay}

## Time in state
${tis || "  - none"}

## Most-repaired tasks
${worst}

## Transitions by role
- ${roles}
`;
}

try {
  const flags = parseFlags(process.argv.slice(2));
  const m = buildMetrics(flags);
  if (flags.format === "json") console.log(JSON.stringify(m, null, 2));
  else process.stdout.write(renderMarkdown(m));
} catch (error) {
  console.error(`autonomy-metrics: ${error.message}`);
  process.exit(1);
}
