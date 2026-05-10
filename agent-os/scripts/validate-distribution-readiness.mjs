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

const homebrew = manifest?.homebrew;
if (!homebrew || typeof homebrew !== "object") {
  fail("package manager manifest must include a homebrew section");
} else {
  if (homebrew.schema !== "covenant.package-manager-manifest-homebrew.v1") {
    fail("homebrew section schema must be covenant.package-manager-manifest-homebrew.v1");
  }
  if (homebrew.status !== "draft_empty_placeholders") {
    fail("homebrew section must remain draft_empty_placeholders");
  }
  if (homebrew.ready_for_homebrew_review !== false) {
    fail("homebrew section must not be ready for review while placeholders are empty");
  }
  if (homebrew.local_paths_allowed !== false) {
    fail("homebrew section must reject local paths");
  }

  const homebrewRequired = [
    "tap",
    "formula_path",
    "formula_class_name",
    "formula_version",
    "artifact_url",
    "artifact_sha256",
    "bottle",
    "caveats",
    "service_definition",
    "test_block",
    "livecheck",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ];
  const homebrewFields = new Set(homebrew.required_fields ?? []);
  for (const field of homebrewRequired) {
    if (!homebrewFields.has(field)) {
      fail(`homebrew section required field missing: ${field}`);
    }
  }

  for (const field of [
    "tap",
    "formula_path",
    "formula_class_name",
    "formula_version",
    "artifact_url",
    "artifact_sha256",
    "caveats",
    "service_definition",
    "test_block",
    "livecheck",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ]) {
    if (homebrew[field] !== null) {
      fail(`homebrew section ${field} must remain null until implemented`);
    }
  }

  const bottle = homebrew.bottle;
  if (!bottle || typeof bottle !== "object") {
    fail("homebrew section must include a bottle placeholder");
  } else {
    if (bottle.enabled !== null) {
      fail("homebrew section bottle.enabled must remain null until decided");
    }
    if (bottle.cellar !== null) {
      fail("homebrew section bottle.cellar must remain null until decided");
    }
    if (!Array.isArray(bottle.bottles) || bottle.bottles.length !== 0) {
      fail("homebrew section bottle.bottles must be an empty array");
    }
  }
}

const nix = manifest?.nix;
if (!nix || typeof nix !== "object") {
  fail("package manager manifest must include a nix section");
} else {
  if (nix.schema !== "covenant.package-manager-manifest-nix.v1") {
    fail("nix section schema must be covenant.package-manager-manifest-nix.v1");
  }
  if (nix.status !== "draft_empty_placeholders") {
    fail("nix section must remain draft_empty_placeholders");
  }
  if (nix.ready_for_nix_review !== false) {
    fail("nix section must not be ready for review while placeholders are empty");
  }
  if (nix.local_paths_allowed !== false) {
    fail("nix section must reject local paths");
  }

  const nixRequired = [
    "flake_source",
    "flake_ref",
    "derivation_path",
    "derivation_hash",
    "package_name",
    "package_version",
    "platforms",
    "binary_path",
    "service_module",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ];
  const nixFields = new Set(nix.required_fields ?? []);
  for (const field of nixRequired) {
    if (!nixFields.has(field)) {
      fail(`nix section required field missing: ${field}`);
    }
  }

  for (const field of [
    "flake_source",
    "flake_ref",
    "derivation_path",
    "derivation_hash",
    "package_name",
    "package_version",
    "binary_path",
    "service_module",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ]) {
    if (nix[field] !== null) {
      fail(`nix section ${field} must remain null until implemented`);
    }
  }

  if (!Array.isArray(nix.platforms) || nix.platforms.length !== 0) {
    fail("nix section platforms must be an empty array until decided");
  }
}

const debian = manifest?.debian;
if (!debian || typeof debian !== "object") {
  fail("package manager manifest must include a debian section");
} else {
  if (debian.schema !== "covenant.package-manager-manifest-debian.v1") {
    fail("debian section schema must be covenant.package-manager-manifest-debian.v1");
  }
  if (debian.status !== "draft_empty_placeholders") {
    fail("debian section must remain draft_empty_placeholders");
  }
  if (debian.ready_for_debian_review !== false) {
    fail("debian section must not be ready for review while placeholders are empty");
  }
  if (debian.local_paths_allowed !== false) {
    fail("debian section must reject local paths");
  }

  const debianRequired = [
    "package_name",
    "package_version",
    "control_metadata",
    "depends",
    "recommends",
    "suggests",
    "architectures",
    "file_ownership",
    "service_unit",
    "postinst",
    "prerm",
    "postrm",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ];
  const debianFields = new Set(debian.required_fields ?? []);
  for (const field of debianRequired) {
    if (!debianFields.has(field)) {
      fail(`debian section required field missing: ${field}`);
    }
  }

  for (const field of [
    "package_name",
    "package_version",
    "file_ownership",
    "service_unit",
    "postinst",
    "prerm",
    "postrm",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ]) {
    if (debian[field] !== null) {
      fail(`debian section ${field} must remain null until implemented`);
    }
  }

  for (const field of ["depends", "recommends", "suggests", "architectures"]) {
    if (!Array.isArray(debian[field]) || debian[field].length !== 0) {
      fail(`debian section ${field} must be an empty array until decided`);
    }
  }

  const control = debian.control_metadata;
  if (!control || typeof control !== "object") {
    fail("debian section must include a control_metadata placeholder");
  } else {
    for (const field of ["section", "priority", "maintainer", "homepage", "description"]) {
      if (control[field] !== null) {
        fail(`debian section control_metadata.${field} must remain null until decided`);
      }
    }
  }
}

const rpm = manifest?.rpm;
if (!rpm || typeof rpm !== "object") {
  fail("package manager manifest must include an rpm section");
} else {
  if (rpm.schema !== "covenant.package-manager-manifest-rpm.v1") {
    fail("rpm section schema must be covenant.package-manager-manifest-rpm.v1");
  }
  if (rpm.status !== "draft_empty_placeholders") {
    fail("rpm section must remain draft_empty_placeholders");
  }
  if (rpm.ready_for_rpm_review !== false) {
    fail("rpm section must not be ready for review while placeholders are empty");
  }
  if (rpm.local_paths_allowed !== false) {
    fail("rpm section must reject local paths");
  }

  const rpmRequired = [
    "package_name",
    "package_version",
    "spec_metadata",
    "requires",
    "recommends",
    "suggests",
    "architectures",
    "file_ownership",
    "service_unit",
    "pre_scriptlet",
    "post_scriptlet",
    "preun_scriptlet",
    "postun_scriptlet",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ];
  const rpmFields = new Set(rpm.required_fields ?? []);
  for (const field of rpmRequired) {
    if (!rpmFields.has(field)) {
      fail(`rpm section required field missing: ${field}`);
    }
  }

  for (const field of [
    "package_name",
    "package_version",
    "file_ownership",
    "service_unit",
    "pre_scriptlet",
    "post_scriptlet",
    "preun_scriptlet",
    "postun_scriptlet",
    "install_check",
    "uninstall_check",
    "upgrade_check",
    "rollback_check",
    "signature_verification",
  ]) {
    if (rpm[field] !== null) {
      fail(`rpm section ${field} must remain null until implemented`);
    }
  }

  for (const field of ["requires", "recommends", "suggests", "architectures"]) {
    if (!Array.isArray(rpm[field]) || rpm[field].length !== 0) {
      fail(`rpm section ${field} must be an empty array until decided`);
    }
  }

  const spec = rpm.spec_metadata;
  if (!spec || typeof spec !== "object") {
    fail("rpm section must include a spec_metadata placeholder");
  } else {
    for (const field of ["summary", "license", "url", "group", "vendor"]) {
      if (spec[field] !== null) {
        fail(`rpm section spec_metadata.${field} must remain null until decided`);
      }
    }
  }
}

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
    "docs/internal/package-manager-readiness.md",
    "agent-os/scripts/package-manager-readiness.mjs",
    "agent-os/scripts/validate-package-manager-readiness.mjs",
    "agent-os/scripts/distribution-readiness.mjs",
    "agent-os/scripts/validate-distribution-readiness.mjs",
    "docs/internal/distribution-readiness.md",
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
