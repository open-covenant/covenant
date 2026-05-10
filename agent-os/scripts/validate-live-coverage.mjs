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

function assertStringArray(value, field, surfaceId) {
  if (!Array.isArray(value) || value.length === 0) {
    fail(`${surfaceId}: ${field} must be a non-empty array`);
  }
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim() === "") {
      fail(`${surfaceId}: ${field} entries must be non-empty strings`);
    }
  }
}

function assertCommand(value, field, surfaceId) {
  if (typeof value !== "string" || value.trim() === "") {
    fail(`${surfaceId}: ${field} must be a non-empty command`);
  }
  const [command, script] = value.split(/\s+/);
  if (command !== "node" || !script) {
    fail(`${surfaceId}: ${field} must be a node script command`);
  }
  assertPath(script, field, surfaceId);
}

function assertPromotion(surface) {
  const promotion = surface.promotion;
  if (!promotion || typeof promotion !== "object") {
    fail(`${surface.id}: promotion metadata is required`);
  }
  assertCommand(promotion.readinessCommand, "promotion.readinessCommand", surface.id);
  assertCommand(promotion.validatorCommand, "promotion.validatorCommand", surface.id);
  if (promotion.requiredCi !== false) {
    fail(`${surface.id}: promotion.requiredCi must remain false until CI promotion is approved`);
  }
  if (promotion.humanApprovalRequired !== true) {
    fail(`${surface.id}: promotion.humanApprovalRequired must be true`);
  }
  assertStringArray(promotion.prerequisites, "promotion.prerequisites", surface.id);
  assertStringArray(promotion.prerequisiteSkips, "promotion.prerequisiteSkips", surface.id);
  assertStringArray(promotion.realFailures, "promotion.realFailures", surface.id);

  const skipText = promotion.prerequisiteSkips.join("\n").toLowerCase();
  const failureText = promotion.realFailures.join("\n").toLowerCase();
  if (!skipText.includes("non-linux") || !skipText.includes("rootfs unset")) {
    fail(`${surface.id}: promotion.prerequisiteSkips must distinguish unsupported host and unset rootfs`);
  }
  if (!failureText.includes("invalid") || !failureText.includes("runsc")) {
    fail(`${surface.id}: promotion.realFailures must distinguish configured rootfs/runsc failures`);
  }
}

function assertScopeCoverage(surface) {
  if (surface.scopeCoverage === undefined) return 0;
  if (!Array.isArray(surface.scopeCoverage) || surface.scopeCoverage.length === 0) {
    fail(`${surface.id}: scopeCoverage must be a non-empty array when present`);
  }

  let count = 0;
  for (const coverage of surface.scopeCoverage) {
    if (!coverage || typeof coverage !== "object") {
      fail(`${surface.id}: scopeCoverage entries must be objects`);
    }
    if (typeof coverage.namespace !== "string" || !coverage.namespace.includes(".")) {
      fail(`${surface.id}: scopeCoverage.namespace must name an action namespace`);
    }
    if (coverage.delegated !== true) {
      fail(`${surface.id}: scopeCoverage.delegated must be true for delegated scope evidence`);
    }
    if (!surface.liveTests.includes(coverage.liveTest)) {
      fail(`${surface.id}: scopeCoverage.liveTest must be listed in liveTests`);
    }
    const source = assertPath(coverage.liveTest, "scopeCoverage.liveTest", surface.id);
    assertLiveTest(coverage.liveTest, source, surface.id);
    assertStringArray(coverage.deniedEvidence, "scopeCoverage.deniedEvidence", surface.id);
    assertStringArray(coverage.allowedEvidence, "scopeCoverage.allowedEvidence", surface.id);
    count += 1;
  }
  return count;
}

const matrix = readJson(matrixPath);
if (matrix.version !== 1) fail("version must be 1");
if (!Array.isArray(matrix.surfaces) || matrix.surfaces.length === 0) {
  fail("surfaces must be a non-empty array");
}

const ids = new Set();
let liveCount = 0;
let mockOnlyCount = 0;
let scopedDelegatedCount = 0;

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

  if (surface.promotion) {
    assertPromotion(surface);
  }
  scopedDelegatedCount += assertScopeCoverage(surface);

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

const gvisor = matrix.surfaces.find((surface) => surface.id === "runtime-linux-gvisor");
if (!gvisor) {
  fail("runtime-linux-gvisor surface is required");
}
assertPromotion(gvisor);

if (scopedDelegatedCount === 0) {
  fail("at least one delegated scoped capability live coverage entry is required");
}

console.log(
  `validate-live-coverage: ok (${matrix.surfaces.length} surfaces, ${liveCount} live test files, ${mockOnlyCount} mock-only surfaces, ${scopedDelegatedCount} scoped delegated entries)`,
);
