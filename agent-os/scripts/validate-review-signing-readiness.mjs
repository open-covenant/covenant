#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function run(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/review-signing-readiness.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function fail(errors) {
  console.error("validate-review-signing-readiness: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const result = run(["--json"]);
if (result.status !== 0) {
  process.stderr.write(result.stderr || result.stdout);
  process.exit(result.status ?? 1);
}

const errors = [];
let report;
try {
  report = JSON.parse(result.stdout);
} catch (error) {
  fail([`output is not JSON: ${error.message}`]);
}

if (report.kind !== "covenant_review_signing_readiness") {
  errors.push("unexpected report kind");
}
if (report.schema !== "covenant.review-signing-readiness.v1") {
  errors.push("unexpected report schema");
}
if (report.ready_for_unsigned_review_artifacts !== true) {
  errors.push("unsigned review artifacts must remain ready");
}
if (report.ready_for_signed_review_artifacts !== false) {
  errors.push("signed review artifacts must remain blocked until human key custody exists");
}
if (report.project_key?.status !== "not_selected") {
  errors.push("project key status must remain not_selected");
}
for (const field of ["key_id", "public_key_spki_sha256", "public_key_source", "custody_policy"]) {
  if (report.project_key?.[field] !== null) {
    errors.push(`project_key.${field} must remain null`);
  }
}
if (report.project_key?.local_key_material_recorded !== false) {
  errors.push("project key material must not be recorded");
}
if (!Array.isArray(report.human_decisions) || report.human_decisions.length < 5) {
  errors.push("human review signing decisions must be explicit");
}
if (!Array.isArray(report.non_goals) || !report.non_goals.some((entry) => /creating a project signing key/i.test(entry))) {
  errors.push("non-goals must prohibit creating a project signing key");
}

function scan(value, pointer = "$") {
  if (typeof value === "string") {
    if (value.startsWith("/") || value.includes("\\") || value.includes("$HOME")) {
      errors.push(`${pointer} must not contain local paths`);
    }
    const forbiddenAttribution = new RegExp(
      [`Co-${"Authored-By"}:`, `${"Generated"} with`, `${"Claude"} Code`].join("|"),
      "i",
    );
    if (forbiddenAttribution.test(value)) {
      errors.push(`${pointer} contains forbidden attribution text`);
    }
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((item, index) => scan(item, `${pointer}[${index}]`));
    return;
  }
  if (value && typeof value === "object") {
    for (const [key, nested] of Object.entries(value)) {
      scan(nested, `${pointer}.${key}`);
    }
  }
}
scan(report.project_key, "$.project_key");

const gates = new Map((report.gates ?? []).map((gate) => [gate.id, gate]));
for (const id of [
  "unsigned-review-artifact-toolchain",
  "review-signing-contract",
  "project-review-key-custody",
  "public-key-publication",
  "rotation-revocation-policy",
  "release-evidence-policy",
]) {
  if (!gates.has(id)) {
    errors.push(`missing gate: ${id}`);
  }
}
for (const id of ["unsigned-review-artifact-toolchain", "review-signing-contract"]) {
  const gate = gates.get(id);
  if (gate?.ok !== true) {
    errors.push(`${id} must pass`);
  }
}
for (const id of [
  "project-review-key-custody",
  "public-key-publication",
  "rotation-revocation-policy",
  "release-evidence-policy",
]) {
  const gate = gates.get(id);
  if (gate?.ok !== false || gate?.human_decision_required !== true) {
    errors.push(`${id} must remain human-required`);
  }
  if (!Array.isArray(gate?.blockers) || gate.blockers.length === 0) {
    errors.push(`${id} must list blockers`);
  }
}

const strict = run(["--json", "--strict-signed"]);
if (strict.status === 0) {
  errors.push("strict signed mode must fail until signed review artifacts are approved");
}

if (errors.length > 0) {
  fail(errors);
}

console.log("validate-review-signing-readiness: ok");
