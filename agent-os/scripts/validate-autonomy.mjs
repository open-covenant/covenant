#!/usr/bin/env node
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const workflowPath = join(root, "autonomy", "workflow.json");
const tasksDir = join(root, "autonomy", "tasks");

const forbiddenPatterns = [
  {
    label: "machine-local home path",
    pattern: new RegExp(`/${"Users"}/[^\\s"'\\]]+`)
  },
  {
    label: "legacy project domain",
    pattern: new RegExp(`covenant${"-?"}base`, "i")
  },
  {
    label: "client-specific project framing",
    pattern: new RegExp(`${"Claude"} ${"Code"}`)
  }
];

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const errors = [];
const fail = (path, message) => {
  errors.push(`${relative(process.cwd(), path)}: ${message}`);
};

const assertArray = (path, value, name, min = 1) => {
  if (!Array.isArray(value)) {
    fail(path, `${name} must be an array`);
    return [];
  }
  if (value.length < min) {
    fail(path, `${name} must contain at least ${min} item(s)`);
  }
  return value;
};

const assertString = (path, value, name) => {
  if (typeof value !== "string" || value.trim() === "") {
    fail(path, `${name} must be a non-empty string`);
    return "";
  }
  return value;
};

const scanForbidden = (path, value, pointer = "$") => {
  if (typeof value === "string") {
    for (const { label, pattern } of forbiddenPatterns) {
      if (pattern.test(value)) {
        fail(path, `${pointer} contains forbidden public identifier: ${label}`);
      }
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => scanForbidden(path, item, `${pointer}[${index}]`));
    return;
  }
  if (value && typeof value === "object") {
    for (const [key, item] of Object.entries(value)) {
      scanForbidden(path, item, `${pointer}.${key}`);
    }
  }
};

let workflow;
try {
  workflow = readJson(workflowPath);
} catch (error) {
  fail(workflowPath, `cannot parse workflow JSON: ${error.message}`);
  workflow = {};
}

scanForbidden(workflowPath, workflow);

const states = new Set(assertArray(workflowPath, workflow.states, "states"));
const roles = new Set(assertArray(workflowPath, workflow.roles, "roles"));
const priorities = new Set(assertArray(workflowPath, workflow.priorities, "priorities"));
const gateNames = new Set(Object.keys(workflow.gates || {}));

if (gateNames.size === 0) {
  fail(workflowPath, "gates must be a non-empty object");
}

for (const [index, transition] of assertArray(
  workflowPath,
  workflow.transitions,
  "transitions"
).entries()) {
  if (!Array.isArray(transition) || transition.length !== 2) {
    fail(workflowPath, `transitions[${index}] must be [from, to]`);
    continue;
  }
  const [from, to] = transition;
  if (!states.has(from)) fail(workflowPath, `transition from unknown state: ${from}`);
  if (!states.has(to)) fail(workflowPath, `transition to unknown state: ${to}`);
}

const taskFiles = readdirSync(tasksDir)
  .filter((name) => name.endsWith(".json"))
  .sort();

if (taskFiles.length === 0) {
  fail(tasksDir, "at least one task JSON file is required");
}

const ids = new Set();
for (const file of taskFiles) {
  const path = join(tasksDir, file);
  let task;
  try {
    task = readJson(path);
  } catch (error) {
    fail(path, `cannot parse task JSON: ${error.message}`);
    continue;
  }

  scanForbidden(path, task);

  const id = assertString(path, task.id, "id");
  if (id) {
    if (!/^[a-z0-9][a-z0-9-]*$/.test(id)) {
      fail(path, "id must be lowercase kebab-case");
    }
    if (ids.has(id)) {
      fail(path, `duplicate task id: ${id}`);
    }
    ids.add(id);
    if (file !== `${id}.json`) {
      fail(path, `filename must match id (${id}.json)`);
    }
  }

  assertString(path, task.title, "title");
  assertString(path, task.summary, "summary");
  assertString(path, task.nextAction, "nextAction");
  assertString(path, task.updated, "updated");

  if (!/^\d{4}-\d{2}-\d{2}$/.test(task.updated || "")) {
    fail(path, "updated must use YYYY-MM-DD");
  }

  if (!states.has(task.state)) {
    fail(path, `state must be one of: ${[...states].join(", ")}`);
  }

  if (!priorities.has(task.priority)) {
    fail(path, `priority must be one of: ${[...priorities].join(", ")}`);
  }

  if (!roles.has(task.ownerRole)) {
    fail(path, `ownerRole must be one of: ${[...roles].join(", ")}`);
  }

  const scope = assertArray(path, task.scope, "scope");
  for (const entry of scope) {
    if (typeof entry !== "string" || entry.startsWith("/") || entry.includes("..")) {
      fail(path, `scope entries must be relative repository paths: ${entry}`);
    }
  }

  const gates = assertArray(path, task.gates, "gates");
  for (const gate of gates) {
    if (!gateNames.has(gate)) {
      fail(path, `unknown gate: ${gate}`);
    }
  }

  assertArray(path, task.verification, "verification");

  const expectedFailureModes = assertArray(
    path,
    task.expectedFailureModes,
    "expectedFailureModes",
    task.state === "proposed" ? 0 : 3
  );
  if (task.state !== "proposed" && expectedFailureModes.length < 3) {
    fail(path, "triaged-or-later tasks must name at least three expected failure modes");
  }

  if (!Array.isArray(task.humanEscalation)) {
    fail(path, "humanEscalation must be an array");
  }
}

if (errors.length > 0) {
  console.error("validate-autonomy: failed");
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}

console.log(`validate-autonomy: ok (${taskFiles.length} task records)`);
