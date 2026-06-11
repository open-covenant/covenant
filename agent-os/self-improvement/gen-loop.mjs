#!/usr/bin/env node
// Derives the public loop-observatory snapshot from the autonomous loop's
// local task ledger. The raw events.jsonl was deliberately cut from the
// public repo; this publishes sanitized aggregates only — state counts,
// throughput, and recent integrations with hard-truncated notes.

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const eventsPath = resolve(here, "..", "..", "..", "covenant", "agent-os", "autonomy", "events.jsonl");
const outPath = resolve(here, "..", "..", "landing", "public", "loop.json");

if (!existsSync(eventsPath)) {
  console.error(`events ledger not found at ${eventsPath} — skipping loop.json`);
  process.exit(0);
}

const events = [];
for (const line of readFileSync(eventsPath, "utf8").split("\n")) {
  if (!line.trim()) continue;
  try { events.push(JSON.parse(line)); } catch {}
}

const latestByTask = new Map();
for (const e of events) latestByTask.set(e.taskId, e);

const states = {};
for (const e of latestByTask.values()) states[e.to] = (states[e.to] ?? 0) + 1;

const integrated = events.filter((e) => e.to === "integrated");
const now = Date.now();
const last7d = integrated.filter((e) => now - Date.parse(e.timestamp) < 7 * 864e5).length;
const last24h = integrated.filter((e) => now - Date.parse(e.timestamp) < 864e5).length;

const ACTIVE = ["in_progress", "self_review", "cross_review", "validation", "ready"];
const inFlight = [...latestByTask.values()]
  .filter((e) => ACTIVE.includes(e.to))
  .sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp))[0] ?? null;

const recent = integrated.slice(-8).reverse().map((e) => ({
  at: e.timestamp,
  task: e.taskId.slice(0, 60),
  note: String(e.note ?? "").replace(/\s+/g, " ").slice(0, 140),
}));

const payload = {
  updatedAt: new Date().toISOString(),
  branch: "loop/main-track",
  totals: { events: events.length, tasks: latestByTask.size, integrated: integrated.length },
  throughput: { last24h, last7d },
  states,
  inFlight: inFlight ? { task: inFlight.taskId.slice(0, 60), state: inFlight.to, since: inFlight.timestamp } : null,
  recent,
};

writeFileSync(outPath, JSON.stringify(payload, null, 2) + "\n");
console.log(`loop.json: ${payload.totals.integrated} integrated, ${last7d} last 7d, in-flight: ${payload.inFlight?.task ?? "none"}`);
