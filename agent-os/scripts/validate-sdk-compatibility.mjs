#!/usr/bin/env node
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

const result = spawnSync(process.execPath, ["agent-os/scripts/sdk-compatibility.mjs", "--json"], {
  cwd: repoRoot,
  encoding: "utf8",
  stdio: ["ignore", "pipe", "pipe"],
});

if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

let report;
try {
  report = JSON.parse(result.stdout);
} catch (error) {
  console.error(`validate-sdk-compatibility: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_sdk_compatibility") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.sdk-compatibility.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_workspace_alpha !== true) {
  fail("workspace alpha SDK metadata must validate");
}
if (report.ready_for_public_sdk !== false) {
  fail("public SDK readiness must remain blocked");
}
if (!existsSync(join(repoRoot, "docs", "sdk-compatibility.md"))) {
  fail("docs/sdk-compatibility.md is required");
}

const packages = new Map((report.packages ?? []).map((pkg) => [pkg.name, pkg]));
const sdk = packages.get("@covenant/sdk");
const sdkUi = packages.get("@covenant/sdk-ui");

if (!sdk || !sdkUi) {
  fail("both SDK package reports are required");
}
if (sdk && sdk.private !== false) {
  fail("@covenant/sdk must stay workspace-only but publishable in metadata");
}
if (sdkUi && sdkUi.private !== true) {
  fail("@covenant/sdk-ui must stay private until public UI stability is approved");
}
for (const pkg of [sdk, sdkUi].filter(Boolean)) {
  if (pkg.ok !== true) {
    fail(`${pkg.name} metadata failed validation`);
  }
  if (pkg.export_map?.root?.default !== pkg.export_map?.main) {
    fail(`${pkg.name} root default export must match main`);
  }
  if (pkg.export_map?.root?.types !== pkg.export_map?.types) {
    fail(`${pkg.name} root types export must match types`);
  }
  if (!Array.isArray(pkg.source_exports) || pkg.source_exports.length === 0) {
    fail(`${pkg.name} must expose at least one source export`);
  }
  if (pkg.fixture?.matches !== true) {
    fail(`${pkg.name} export fixture must match source exports`);
  }
}

if (!report.blockers.some((blocker) => /npm publication/i.test(blocker))) {
  fail("public SDK blockers must include npm publication");
}
if (!report.blockers.some((blocker) => /protocol bindings/i.test(blocker))) {
  fail("public SDK blockers must include protocol binding fixtures");
}
if (report.instruction_fixture?.matches !== true) {
  fail("SDK instruction fixture must match source descriptors");
}
if (!Array.isArray(report.instruction_fixture?.descriptors) || report.instruction_fixture.descriptors.length < 4) {
  fail("SDK instruction fixture must include the current descriptor surface");
}

if (errors.length > 0) {
  console.error("validate-sdk-compatibility: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-sdk-compatibility: ok");
