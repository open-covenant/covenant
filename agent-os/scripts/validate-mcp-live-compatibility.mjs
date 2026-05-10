#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function fail(message) {
  console.error(`validate-mcp-live-compatibility: ${message}`);
  process.exit(1);
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

const fakeServer = read("agent-os/crates/covenant-mcp/src/bin/fake_server.rs");
const liveTest = read("agent-os/crates/covenant-mcp/tests/live_stdio.rs");
const statusAbs = join(repoRoot, "docs/internal/status.md");
const status = existsSync(statusAbs) ? readFileSync(statusAbs, "utf8") : null;
const liveCoverage = read("agent-os/autonomy/live-coverage.json");

for (const needle of [
  "--multi-tool",
  "\"name\": \"sum\"",
  "\"name\": \"fail\"",
  "\"type\": \"json\"",
  "\"isError\": true",
]) {
  if (!fakeServer.includes(needle)) {
    fail(`fake server missing ${needle}`);
  }
}

for (const needle of [
  "live_stdio_mcp_handles_multi_tool_json_and_error_results",
  "--multi-tool",
  "Content::Json",
  "fail_result.is_error",
]) {
  if (!liveTest.includes(needle)) {
    fail(`live stdio test missing ${needle}`);
  }
}

if (!liveCoverage.includes("multi-tool stdio compatibility")) {
  fail("live coverage matrix must mention multi-tool stdio compatibility");
}
if (status !== null && !status.includes("multi-tool stdio live compatibility")) {
  fail("status matrix must mention multi-tool stdio live compatibility");
}

console.log("validate-mcp-live-compatibility: ok");
