#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const repoRoot = resolve(root, "..");
const tasksDir = join(root, "autonomy", "tasks");
const eventsPath = join(root, "autonomy", "events.jsonl");
const stateOrder = [
  "proposed",
  "triaged",
  "planned",
  "in_progress",
  "self_review",
  "cross_review",
  "validation",
  "repair",
  "ready",
  "integrated",
  "blocked",
];
const priorityOrder = ["critical", "high", "medium", "low"];

function usage() {
  console.error(`usage:
  node agent-os/scripts/autonomy-summary.mjs [--since YYYY-MM-DD] [--format markdown|json] [--limit N]`);
}

function parseFlags(args) {
  const flags = { since: null, format: "markdown", limit: 12 };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    const value = args[index + 1];
    if (!value || value.startsWith("--")) {
      usage();
      process.exit(2);
    }
    index += 1;
    if (arg === "--since") {
      if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) {
        throw new Error("--since must use YYYY-MM-DD");
      }
      flags.since = value;
    } else if (arg === "--format") {
      if (!["markdown", "json"].includes(value)) {
        throw new Error("--format must be markdown or json");
      }
      flags.format = value;
    } else if (arg === "--limit") {
      const limit = Number.parseInt(value, 10);
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
        throw new Error("--limit must be an integer from 1 to 100");
      }
      flags.limit = limit;
    } else {
      usage();
      process.exit(2);
    }
  }
  return flags;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function readTasks() {
  return readdirSync(tasksDir)
    .filter((name) => name.endsWith(".json"))
    .sort()
    .map((name) => readJson(join(tasksDir, name)));
}

function readEvents() {
  return readFileSync(eventsPath, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim().length > 0)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        throw new Error(`${relative(repoRoot, eventsPath)}:${index + 1}: ${error.message}`);
      }
    });
}

function byCount(items, key) {
  return items.reduce((acc, item) => {
    const value = item[key] ?? "unknown";
    acc[value] = (acc[value] ?? 0) + 1;
    return acc;
  }, {});
}

function compareTasks(a, b) {
  const priority = priorityOrder.indexOf(a.priority) - priorityOrder.indexOf(b.priority);
  if (priority !== 0) return priority;
  if (a.updated !== b.updated) return b.updated.localeCompare(a.updated);
  return a.id.localeCompare(b.id);
}

function compactTask(task, latestEvent) {
  return {
    id: task.id,
    title: task.title,
    state: task.state,
    priority: task.priority,
    updated: task.updated,
    ownerRole: task.ownerRole,
    nextAction: task.nextAction,
    lastTransition: latestEvent
      ? {
          timestamp: latestEvent.timestamp,
          from: latestEvent.from,
          to: latestEvent.to,
          actorRole: latestEvent.actorRole,
          note: latestEvent.note,
        }
      : null,
  };
}

function buildSummary(flags) {
  const tasks = readTasks();
  const events = readEvents();
  const cutoff = flags.since ? `${flags.since}T00:00:00.000Z` : null;
  const scopedEvents = cutoff
    ? events.filter((event) => String(event.timestamp) >= cutoff)
    : events;
  const touchedTaskIds = new Set(scopedEvents.map((event) => event.taskId));
  const scopedTasks = cutoff
    ? tasks.filter((task) => touchedTaskIds.has(task.id))
    : tasks;
  const latestEventByTask = new Map();
  for (const event of events) {
    latestEventByTask.set(event.taskId, event);
  }

  const activeStates = new Set([
    "proposed",
    "triaged",
    "planned",
    "in_progress",
    "self_review",
    "cross_review",
    "validation",
    "repair",
    "ready",
  ]);

  const compact = (task) => compactTask(task, latestEventByTask.get(task.id));
  const compareRecent = (a, b) => {
    const aTime = latestEventByTask.get(a.id)?.timestamp ?? "";
    const bTime = latestEventByTask.get(b.id)?.timestamp ?? "";
    const time = String(bTime).localeCompare(String(aTime));
    return time === 0 ? compareTasks(a, b) : time;
  };
  const active = scopedTasks.filter((task) => activeStates.has(task.state)).sort(compareTasks);
  const blocked = scopedTasks.filter((task) => task.state === "blocked").sort(compareTasks);
  const integrated = scopedTasks.filter((task) => task.state === "integrated").sort(compareRecent);
  const recentEvents = scopedEvents.slice(-flags.limit).reverse();

  return {
    schema: "covenant.autonomy-summary.v1",
    generatedAt: scopedEvents.at(-1)?.timestamp ?? null,
    since: flags.since,
    totals: {
      tasks: scopedTasks.length,
      allTasks: tasks.length,
      events: scopedEvents.length,
      allEvents: events.length,
      byState: Object.fromEntries(
        stateOrder.map((state) => [state, scopedTasks.filter((task) => task.state === state).length]),
      ),
      byPriority: byCount(scopedTasks, "priority"),
    },
    active: active.map(compact),
    blocked: blocked.map(compact),
    integrated: integrated.slice(0, flags.limit).map(compact),
    recentEvents,
  };
}

function renderTaskList(tasks) {
  if (tasks.length === 0) return "- none";
  return tasks
    .map((task) => `- ${task.id} (${task.priority}, ${task.state}): ${task.nextAction}`)
    .join("\n");
}

function renderEvents(events) {
  if (events.length === 0) return "- none";
  return events
    .map(
      (event) =>
        `- ${event.timestamp} ${event.taskId}: ${event.from ?? "null"} -> ${event.to} (${event.actorRole})`,
    )
    .join("\n");
}

function renderMarkdown(summary) {
  const scope = summary.since ? ` since ${summary.since}` : "";
  const stateCounts = stateOrder
    .filter((state) => summary.totals.byState[state] > 0)
    .map((state) => `${state}: ${summary.totals.byState[state]}`)
    .join(", ");

  return `# Autonomy Summary${scope}

- generated: ${summary.generatedAt}
- tasks: ${summary.totals.tasks} scoped / ${summary.totals.allTasks} total
- events: ${summary.totals.events} scoped / ${summary.totals.allEvents} total
- states: ${stateCounts || "none"}

## Active

${renderTaskList(summary.active)}

## Blocked

${renderTaskList(summary.blocked)}

## Recently Integrated

${renderTaskList(summary.integrated)}

## Recent Events

${renderEvents(summary.recentEvents)}
`;
}

try {
  const flags = parseFlags(process.argv.slice(2));
  const summary = buildSummary(flags);
  if (flags.format === "json") {
    console.log(JSON.stringify(summary, null, 2));
  } else {
    process.stdout.write(renderMarkdown(summary));
  }
} catch (error) {
  console.error(`autonomy-summary: ${error.message}`);
  process.exit(1);
}
