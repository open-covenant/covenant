#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const repoRoot = resolve(root, "..");
const tasksDir = join(root, "autonomy", "tasks");
const eventsPath = join(root, "autonomy", "events.jsonl");

function usage() {
  console.log(`usage: autonomy-review-artifact <task-id> [--json]

Emit an unsigned review artifact for one autonomy task without mutating files or Git metadata.`);
}

function fail(message, code = 1) {
  console.error(message);
  process.exit(code);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.includes("--json");
const positional = args.filter((arg) => arg !== "--json");
if (positional.length !== 1) {
  usage();
  process.exit(2);
}

const taskId = positional[0];
if (!/^[a-z0-9][a-z0-9-]*$/.test(taskId)) {
  fail("autonomy-review-artifact: task id must be lowercase kebab-case", 2);
}

const taskPath = join(tasksDir, `${taskId}.json`);
if (!existsSync(taskPath)) {
  fail(`autonomy-review-artifact: unknown task: ${taskId}`);
}

let taskText;
let task;
try {
  taskText = readFileSync(taskPath, "utf8").replace(/\r\n/g, "\n");
  task = JSON.parse(taskText);
} catch (error) {
  fail(`autonomy-review-artifact: cannot read task: ${error.message}`);
}

const eventLines = existsSync(eventsPath)
  ? readFileSync(eventsPath, "utf8").replace(/\r\n/g, "\n").split("\n").filter(Boolean)
  : [];

const events = [];
const selectedEventLines = [];
for (const line of eventLines) {
  let event;
  try {
    event = JSON.parse(line);
  } catch (error) {
    fail(`autonomy-review-artifact: invalid event JSON: ${error.message}`);
  }
  if (event.taskId === taskId) {
    events.push(event);
    selectedEventLines.push(line);
  }
}

const artifact = {
  kind: "autonomy_review_artifact",
  schema_version: 1,
  generated_at: new Date().toISOString(),
  source: {
    task_path: relative(repoRoot, taskPath),
    events_path: relative(repoRoot, eventsPath),
  },
  task: {
    id: task.id,
    title: task.title,
    state: task.state,
    priority: task.priority,
    owner_role: task.ownerRole,
    updated: task.updated,
    scope: task.scope,
    gates: task.gates,
    verification: task.verification,
    human_escalation: task.humanEscalation,
    expected_failure_modes: task.expectedFailureModes,
  },
  review_trace: {
    event_count: events.length,
    first_event_at: events[0]?.timestamp ?? null,
    last_event_at: events.at(-1)?.timestamp ?? null,
    events,
  },
  digests: {
    task_sha256: sha256(taskText),
    event_subset_sha256: sha256(selectedEventLines.join("\n") + (selectedEventLines.length > 0 ? "\n" : "")),
  },
  signing: {
    status: "unsigned",
    reason: "review artifact signing and key custody are not implemented",
  },
};

if (asJson) {
  console.log(JSON.stringify(artifact, null, 2));
} else {
  console.log(`autonomy review artifact: ${taskId}`);
  console.log(`state: ${artifact.task.state}`);
  console.log(`priority: ${artifact.task.priority}`);
  console.log(`events: ${artifact.review_trace.event_count}`);
  console.log(`task sha256: ${artifact.digests.task_sha256}`);
  console.log(`event subset sha256: ${artifact.digests.event_subset_sha256}`);
  console.log(`signing: ${artifact.signing.status}`);
  console.log("");
  console.log("verification:");
  for (const command of artifact.task.verification || []) {
    console.log(`  - ${command}`);
  }
}
