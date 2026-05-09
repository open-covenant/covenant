#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentRoot = resolve(here, "..");
const repoRoot = resolve(agentRoot, "..");
const matrixPath = join(agentRoot, "autonomy", "live-coverage.json");

const allowedStatuses = new Set(["covered", "mock_only", "external_service", "planned"]);

function fail(message) {
  console.error(`validate-live-coverage: ${message}`);
  process.exit(1);
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${path}: ${error.message}`);
  }
}

function assertPath(path, field, surfaceId) {
  if (typeof path !== "string" || path.length === 0) {
    fail(`${surfaceId}: ${field} entries must be non-empty strings`);
  }
  if (path.startsWith("/") || path.includes("..")) {
    fail(`${surfaceId}: ${field} path must be repository-relative: ${path}`);
  }
  const absolute = join(repoRoot, path);
  if (!existsSync(absolute)) {
    fail(`${surfaceId}: missing ${field} path ${path}`);
  }
  return readFileSync(absolute, "utf8");
}

function assertLiveTest(path, source, surfaceId) {
  if (!/\/live_[^/]+\.rs$/.test(path)) {
    fail(`${surfaceId}: live test path must use live_*.rs naming: ${path}`);
  }
  if (!source.includes("#[ignore")) {
    fail(`${surfaceId}: live test must be opt-in with #[ignore]: ${path}`);
  }
  if (!/fn\s+live_[A-Za-z0-9_]+\s*\(/.test(source)) {
    fail(`${surfaceId}: live test file must define a live_* test function: ${path}`);
  }
}

const matrix = readJson(matrixPath);
if (matrix.version !== 1) fail("version must be 1");
if (!Array.isArray(matrix.surfaces) || matrix.surfaces.length === 0) {
  fail("surfaces must be a non-empty array");
}

const ids = new Set();
let liveCount = 0;
let mockOnlyCount = 0;

for (const surface of matrix.surfaces) {
  if (!surface || typeof surface !== "object") fail("surface must be an object");
  if (!surface.id || typeof surface.id !== "string") fail("surface id is required");
  if (ids.has(surface.id)) fail(`duplicate surface id: ${surface.id}`);
  ids.add(surface.id);
  if (!surface.name || typeof surface.name !== "string") {
    fail(`${surface.id}: name is required`);
  }
  if (!allowedStatuses.has(surface.status)) {
    fail(`${surface.id}: unsupported status ${surface.status}`);
  }
  if (!Array.isArray(surface.mockTests) || !Array.isArray(surface.liveTests)) {
    fail(`${surface.id}: mockTests and liveTests must be arrays`);
  }
  if (surface.mockTests.length === 0 && surface.liveTests.length === 0) {
    fail(`${surface.id}: at least one mock or live test path is required`);
  }
  if (!surface.nextGap || typeof surface.nextGap !== "string") {
    fail(`${surface.id}: nextGap is required`);
  }

  for (const path of surface.mockTests) {
    assertPath(path, "mockTests", surface.id);
  }
  for (const path of surface.liveTests) {
    const source = assertPath(path, "liveTests", surface.id);
    assertLiveTest(path, source, surface.id);
  }

  if (surface.liveTests.length === 0) {
    mockOnlyCount += 1;
  } else {
    liveCount += surface.liveTests.length;
  }
}

console.log(
  `validate-live-coverage: ok (${matrix.surfaces.length} surfaces, ${liveCount} live test files, ${mockOnlyCount} mock-only surfaces)`,
);
