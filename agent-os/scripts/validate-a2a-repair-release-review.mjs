#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");

function run(args) {
  return spawnSync(process.execPath, ["agent-os/scripts/a2a-repair-release-review.mjs", ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function parseJson(result, label) {
  if (result.status !== 0) {
    throw new Error(`${label} failed: ${result.stderr || result.stdout}`);
  }
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new Error(`${label} did not emit JSON: ${error.message}`);
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

const marker = {
  schema: "covenant.a2a-delegated-repair-release-review.v1",
  decision: "accepted",
  review_id: "a2a-delegated-repair-alpha-review",
  approved_by: "Mizuki",
  approved_at: "2026-05-10T00:00:00.000Z",
  scope: {
    automation: "a2a.delegated-repair",
    actions: ["a2a.repair.requeue", "a2a.repair.force_error"],
    task_binding: "required",
    lease_binding: "required",
    counterparty_binding: "required",
    duplicate_risk_policy: "explicit-scope-only",
  },
  conditions: [
    "capability scope validation remains enabled",
    "peer counterparty scope remains mandatory",
    "duplicate-risk posture remains explicit for requeue",
  ],
  evidence: [
    "docs/internal/decisions/0005-a2a-delegated-repair-release-review.md",
    "docs/a2a-repair-authorization.md",
    "docs/a2a-repair-visibility.md",
    "agent-os/scripts/validate-a2a-repair-authorization.mjs",
    "agent-os/scripts/validate-a2a-repair-release-review.mjs",
  ],
};

const tmp = mkdtempSync(join(tmpdir(), "covenant-a2a-review-"));
try {
  const defaultReport = parseJson(run(["--json"]), "default release review report");
  assert(defaultReport.kind === "covenant_a2a_repair_release_review", "unexpected report kind");
  assert(defaultReport.schema === "covenant.a2a-repair-release-review.v1", "unexpected report schema");
  assert(defaultReport.ready_for_delegated_repair_automation === false, "default report must block delegated repair");
  assert(defaultReport.status === "human_required", "default report must remain human_required");
  assert(defaultReport.human_decision_required === true, "default report must require a human decision");
  assert(defaultReport.review_marker?.schema === marker.schema, "marker schema reference missing");
  assert(defaultReport.review_marker?.default_path === "docs/a2a-delegated-repair-release-review.json", "default marker path missing");
  assert(defaultReport.review_marker?.present === false, "default marker must be absent");
  assert(defaultReport.blockers.some((blocker) => blocker.includes("review marker is missing")), "missing marker blocker absent");

  const strictDefault = run(["--json", "--strict"]);
  assert(strictDefault.status !== 0, "strict mode must fail without a valid marker");

  const acceptedMarker = join(tmp, "accepted.json");
  writeFileSync(acceptedMarker, `${JSON.stringify(marker, null, 2)}\n`);
  const acceptedReport = parseJson(run(["--json", "--strict", "--marker", acceptedMarker]), "accepted marker report");
  assert(acceptedReport.ready_for_delegated_repair_automation === true, "accepted marker should pass strict mode");
  assert(acceptedReport.status === "review_accepted", "accepted marker status mismatch");
  assert(acceptedReport.review_marker?.source === "external-review-marker", "external marker source must be redacted");
  assert(acceptedReport.accepted_scope?.counterparty_binding === "required", "counterparty binding missing from accepted scope");
  assert(acceptedReport.accepted_scope?.duplicate_risk_policy === "explicit-scope-only", "duplicate-risk policy missing");

  const invalidMarker = join(tmp, "invalid.json");
  writeFileSync(invalidMarker, `${JSON.stringify({ ...marker, schema: "bad.schema", evidence: [] }, null, 2)}\n`);
  const invalidReport = parseJson(run(["--json", "--marker", invalidMarker]), "invalid marker report");
  assert(invalidReport.ready_for_delegated_repair_automation === false, "invalid marker must not pass");
  assert(invalidReport.status === "human_required", "invalid marker must remain human_required");
  assert(invalidReport.review_marker?.valid === false, "invalid marker validity must be false");
  assert(invalidReport.blockers.some((blocker) => blocker.includes("schema must be")), "schema blocker absent");
  assert(invalidReport.blockers.some((blocker) => blocker.includes("evidence must")), "evidence blocker absent");
} catch (error) {
  console.error(`validate-a2a-repair-release-review: ${error.message}`);
  process.exit(1);
} finally {
  rmSync(tmp, { recursive: true, force: true });
}

console.log("validate-a2a-repair-release-review: ok");
