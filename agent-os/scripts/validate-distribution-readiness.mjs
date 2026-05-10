#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

const result = spawnSync(
  process.execPath,
  ["agent-os/scripts/distribution-readiness.mjs", "--json"],
  {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  },
);

if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

let report;
try {
  report = JSON.parse(result.stdout);
} catch (error) {
  console.error(`validate-distribution-readiness: output is not JSON: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

if (report.kind !== "covenant_distribution_readiness") {
  fail("unexpected report kind");
}
if (report.schema !== "covenant.distribution-readiness.v1") {
  fail("unexpected report schema");
}
if (report.ready_for_source_alpha !== true) {
  fail("source alpha install gate must be ready");
}
if (report.ready_for_public_distribution !== false) {
  fail("public distribution must remain blocked until package, signing, SDK, and upgrade gates are implemented");
}
if (!Array.isArray(report.gates)) {
  fail("gates must be an array");
}
if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 4) {
  fail("human decisions must be explicit");
}

const manifest = report.package_manager_manifest;
if (!manifest || manifest.schema !== "covenant.package-manager-manifest.v1") {
  fail("package manager manifest schema must be covenant.package-manager-manifest.v1");
}
if (manifest?.status !== "draft_empty_placeholders") {
  fail("package manager manifest must remain draft_empty_placeholders");
}
if (manifest?.ready_for_manifest_review !== false) {
  fail("package manager manifest must not be ready for review while placeholders are empty");
}
if (manifest?.manifest_path !== null) {
  fail("package manager manifest path must remain null until a manifest is checked in");
}
if (manifest?.redaction?.machine_local_paths_allowed !== false) {
  fail("package manager manifest must reject machine-local paths");
}
if (manifest?.redaction?.generated_from_local_state !== false) {
  fail("package manager manifest must not be generated from local state");
}

const manifestFields = new Set(manifest?.required_fields ?? []);
for (const field of [
  "channel",
  "package_name",
  "manifest_path",
  "artifact_url",
  "artifact_sha256",
  "signature_verification",
  "install_check",
  "uninstall_check",
  "upgrade_check",
  "rollback_check",
]) {
  if (!manifestFields.has(field)) {
    fail(`package manager manifest required field missing: ${field}`);
  }
}

const channels = new Map((manifest?.channels ?? []).map((channel) => [channel.channel, channel]));
for (const id of ["homebrew", "nix", "debian", "rpm"]) {
  const channel = channels.get(id);
  if (!channel) {
    fail(`package manager manifest missing channel: ${id}`);
    continue;
  }
  if (channel.status !== "placeholder") {
    fail(`package manager manifest channel must remain placeholder: ${id}`);
  }
  for (const field of [
    "package_name",
    "manifest_path",
    "artifact_url",
    "artifact_sha256",
    "signature_verification",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
  ]) {
    if (channel[field] !== null) {
      fail(`package manager manifest ${id}.${field} must remain null until implemented`);
    }
  }
  if (channel.local_paths_allowed !== false) {
    fail(`package manager manifest ${id} must reject local paths`);
  }
}

function visit(value, path = "package_manager_manifest") {
  if (typeof value === "string") {
    if (value.startsWith("/") || value.includes("\\") || value.includes("$HOME")) {
      fail(`${path} must not contain local paths`);
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => visit(item, `${path}[${index}]`));
    return;
  }
  if (value && typeof value === "object") {
    for (const [key, nested] of Object.entries(value)) {
      visit(nested, `${path}.${key}`);
    }
  }
}
visit(manifest);

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "source-alpha-install",
  "source-upgrade-preflight",
  "source-rollback-checkpoint",
  "sdk-compatibility-policy",
  "package-manager-distribution",
  "signed-release-artifacts",
  "sdk-stability",
  "upgrade-policy",
]) {
  if (!gates.has(id)) {
    fail(`missing gate: ${id}`);
  }
}

const source = gates.get("source-alpha-install");
if (source && source.ok !== true) {
  fail("source alpha install gate must pass");
}

const sourceUpgrade = gates.get("source-upgrade-preflight");
if (sourceUpgrade && sourceUpgrade.ok !== true) {
  fail("source upgrade preflight gate must pass");
}
if (
  sourceUpgrade &&
  !sourceUpgrade.evidence.some((item) => item.endsWith("source-install-upgrade-plan.mjs"))
) {
  fail("source upgrade preflight must list its report script as evidence");
}

const sourceRollback = gates.get("source-rollback-checkpoint");
if (sourceRollback && sourceRollback.ok !== true) {
  fail("source rollback checkpoint gate must pass");
}
if (
  sourceRollback &&
  !sourceRollback.evidence.some((item) => item.endsWith("source-install-rollback.mjs"))
) {
  fail("source rollback checkpoint must list its rollback script as evidence");
}

const sdkCompatibility = gates.get("sdk-compatibility-policy");
if (sdkCompatibility && sdkCompatibility.ok !== true) {
  fail("sdk compatibility policy gate must pass");
}
if (
  sdkCompatibility &&
  !sdkCompatibility.evidence.some((item) => item.endsWith("sdk-compatibility.mjs"))
) {
  fail("sdk compatibility policy must list its report script as evidence");
}

const packageManager = gates.get("package-manager-distribution");
if (packageManager) {
  if (packageManager.status !== "documented") {
    fail("package-manager-distribution must report documented local evidence");
  }
  for (const evidence of [
    "docs/package-manager-readiness.md",
    "agent-os/scripts/package-manager-readiness.mjs",
    "agent-os/scripts/validate-package-manager-readiness.mjs",
    "agent-os/scripts/distribution-readiness.mjs",
    "agent-os/scripts/validate-distribution-readiness.mjs",
    "docs/distribution-readiness.md",
  ]) {
    if (!Array.isArray(packageManager.evidence) || !packageManager.evidence.includes(evidence)) {
      fail(`package-manager-distribution must include evidence: ${evidence}`);
    }
  }
  if (!packageManager.blockers.some((blocker) => /draft placeholders/i.test(blocker))) {
    fail("package-manager-distribution must keep draft manifest placeholders blocked");
  }
}

for (const id of [
  "package-manager-distribution",
  "signed-release-artifacts",
  "sdk-stability",
  "upgrade-policy",
]) {
  const gate = gates.get(id);
  if (!gate) continue;
  if (gate.ok !== false) {
    fail(`${id} must not be reported ready yet`);
  }
  if (!Array.isArray(gate.blockers) || gate.blockers.length === 0) {
    fail(`${id} must list blockers`);
  }
  if (gate.human_decision_required !== true) {
    fail(`${id} must record human-owned publication authority`);
  }
}

const signed = gates.get("signed-release-artifacts");
if (
  signed &&
  !signed.blockers.some((blocker) => /project signing key custody/i.test(blocker))
) {
  fail("signed-release-artifacts must call out project signing key custody");
}

const upgrade = gates.get("upgrade-policy");
if (
  upgrade &&
  !upgrade.blockers.some((blocker) => /package-manager rollback/i.test(blocker))
) {
  fail("upgrade-policy must keep public package rollback blocked");
}

const sdk = gates.get("sdk-stability");
if (
  sdk &&
  !sdk.blockers.some((blocker) => /generated protocol bindings/i.test(blocker))
) {
  fail("sdk-stability must keep generated protocol binding fixtures blocked");
}

if (errors.length > 0) {
  console.error("validate-distribution-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-distribution-readiness: ok");
