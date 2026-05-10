#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentRoot = resolve(here, "..");
const repoRoot = resolve(agentRoot, "..");
const matrixPath = join(agentRoot, "autonomy", "privileged-cli-live-matrix.json");
const allowedStatuses = new Set(["covered", "deferred"]);

const expectedCommands = [
  "intent",
  "memory recent",
  "memory search",
  "memory purge",
  "memory compact",
  "memory plan-compaction",
  "memory plan-receipt-backfill",
  "memory repair detach-parent",
  "memory repair delete",
  "memory repair backfill-provenance",
  "capabilities recent",
  "capabilities grant",
  "capabilities revoke",
  "capabilities purge",
  "receipts recent",
  "chain status",
  "chain flush-receipts",
  "chain receipt-batches",
  "chain register-agent",
  "chain stake",
  "chain buy-credits",
  "verify",
  "tools list",
  "tools call",
  "audit recent",
  "audit verify",
  "audit purge",
  "a2a status",
  "a2a requeue",
  "a2a force-error",
  "a2a retry-stale",
  "a2a compact",
  "peers purge",
  "peers rotate",
  "peers list",
  "peers revoke",
  "intents resume",
];

function fail(message) {
  console.error(`validate-privileged-cli-live-matrix: ${message}`);
  process.exit(1);
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${path}: ${error.message}`);
  }
}

function assertLiveTest(path, command) {
  if (typeof path !== "string" || path.trim() === "") {
    fail(`${command}: liveTests entries must be non-empty strings`);
  }
  if (path.startsWith("/") || path.includes("..")) {
    fail(`${command}: live test path must be repository-relative: ${path}`);
  }
  if (!/\/live_[^/]+\.rs$/.test(path)) {
    fail(`${command}: live test path must use live_*.rs naming: ${path}`);
  }
  const absolute = join(repoRoot, path);
  if (!existsSync(absolute)) {
    fail(`${command}: missing live test path ${path}`);
  }
  const source = readFileSync(absolute, "utf8");
  if (!source.includes("#[ignore")) {
    fail(`${command}: live test must be opt-in with #[ignore]: ${path}`);
  }
  if (!/fn\s+live_[A-Za-z0-9_]+\s*\(/.test(source)) {
    fail(`${command}: live test file must define a live_* test function: ${path}`);
  }
}

const matrix = readJson(matrixPath);
if (matrix.version !== 1) fail("version must be 1");
if (!Array.isArray(matrix.commands) || matrix.commands.length === 0) {
  fail("commands must be a non-empty array");
}

const seen = new Set();
let covered = 0;
let deferred = 0;

for (const row of matrix.commands) {
  if (!row || typeof row !== "object") fail("command rows must be objects");
  if (typeof row.command !== "string" || row.command.trim() === "") {
    fail("command row is missing command");
  }
  if (seen.has(row.command)) fail(`duplicate command: ${row.command}`);
  seen.add(row.command);
  if (!allowedStatuses.has(row.status)) {
    fail(`${row.command}: unsupported status ${row.status}`);
  }
  if (!Array.isArray(row.liveTests)) {
    fail(`${row.command}: liveTests must be an array`);
  }
  if (row.status === "covered") {
    covered += 1;
    if (row.liveTests.length === 0) {
      fail(`${row.command}: covered commands must name at least one live test`);
    }
    for (const path of row.liveTests) assertLiveTest(path, row.command);
  } else {
    deferred += 1;
    if (row.liveTests.length !== 0) {
      fail(`${row.command}: deferred commands must not carry live tests`);
    }
    if (typeof row.reason !== "string" || row.reason.trim() === "") {
      fail(`${row.command}: deferred commands must explain reason`);
    }
    if (typeof row.nextGap !== "string" || row.nextGap.trim() === "") {
      fail(`${row.command}: deferred commands must name nextGap`);
    }
  }
}

for (const command of expectedCommands) {
  if (!seen.has(command)) fail(`missing command: ${command}`);
}
for (const command of seen) {
  if (!expectedCommands.includes(command)) fail(`unexpected command: ${command}`);
}

console.log(
  `validate-privileged-cli-live-matrix: ok (${covered} covered, ${deferred} deferred)`,
);
