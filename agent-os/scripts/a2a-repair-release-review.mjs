#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");

const reportSchema = "covenant.a2a-repair-release-review.v1";
const markerSchema = "covenant.a2a-delegated-repair-release-review.v1";
const defaultMarker = "docs/a2a-delegated-repair-release-review.json";
const allowedActions = new Set(["a2a.repair.requeue", "a2a.repair.force_error"]);
const requiredEvidence = [
  "docs/internal/decisions/0005-a2a-delegated-repair-release-review.md",
  "docs/a2a-repair-authorization.md",
  "docs/a2a-repair-visibility.md",
  "agent-os/scripts/validate-a2a-repair-authorization.mjs",
  "agent-os/scripts/validate-a2a-repair-release-review.mjs",
];

function usage() {
  console.log(`usage: a2a-repair-release-review [--json] [--strict] [--marker path]

Report whether delegated A2A repair automation has an explicit human release
review marker. The command is read-only; it does not repair tasks, start daemons,
grant capabilities, or mutate peer state.

Default mode reports the missing marker as a human-required blocker and exits 0.
Use --strict in automation that would enable delegated repair; strict mode exits
non-zero until a valid marker is supplied.`);
}

function parseArgs(argv) {
  const options = {
    asJson: false,
    strict: false,
    markerPath: defaultMarker,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      usage();
      process.exit(0);
    }
    if (arg === "--json") {
      options.asJson = true;
      continue;
    }
    if (arg === "--strict") {
      options.strict = true;
      continue;
    }
    if (arg === "--marker") {
      options.markerPath = argv[index + 1] ?? "";
      index += 1;
      continue;
    }
    usage();
    process.exit(2);
  }

  if (!options.markerPath) {
    usage();
    process.exit(2);
  }

  return options;
}

function resolveMarkerPath(path) {
  return isAbsolute(path) ? resolve(path) : resolve(repoRoot, path);
}

function publicMarkerSource(path) {
  const absolute = resolveMarkerPath(path);
  const rel = relative(repoRoot, absolute);
  if (!rel.startsWith("..") && !isAbsolute(rel)) {
    return rel;
  }
  return "external-review-marker";
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validIsoDate(value) {
  if (typeof value !== "string" || value.length === 0) return false;
  return Number.isFinite(Date.parse(value));
}

function pathExists(path) {
  if (typeof path !== "string") return false;
  if (path.startsWith("/") || path.includes("..") || path.includes("\\")) return false;
  return existsSync(join(repoRoot, path));
}

function validateMarker(marker) {
  const errors = [];
  const fail = (message) => errors.push(message);

  if (!isPlainObject(marker)) {
    return {
      ok: false,
      errors: ["review marker must be a JSON object"],
      acceptedScope: null,
    };
  }

  if (marker.schema !== markerSchema) {
    fail(`schema must be ${markerSchema}`);
  }
  if (marker.decision !== "accepted") {
    fail("decision must be accepted");
  }
  for (const key of ["review_id", "approved_by"]) {
    if (typeof marker[key] !== "string" || marker[key].trim() === "") {
      fail(`${key} must be a non-empty string`);
    }
  }
  if (!validIsoDate(marker.approved_at)) {
    fail("approved_at must be an ISO timestamp");
  }
  if (typeof marker.approved_by === "string" && marker.approved_by.includes("/")) {
    fail("approved_by must use a neutral project alias");
  }

  const scope = marker.scope;
  if (!isPlainObject(scope)) {
    fail("scope must be an object");
  } else {
    if (scope.automation !== "a2a.delegated-repair") {
      fail("scope.automation must be a2a.delegated-repair");
    }
    if (scope.task_binding !== "required") {
      fail("scope.task_binding must be required");
    }
    if (scope.lease_binding !== "required") {
      fail("scope.lease_binding must be required");
    }
    if (scope.counterparty_binding !== "required") {
      fail("scope.counterparty_binding must be required");
    }
    if (scope.duplicate_risk_policy !== "explicit-scope-only") {
      fail("scope.duplicate_risk_policy must be explicit-scope-only");
    }
    if (!Array.isArray(scope.actions) || scope.actions.length === 0) {
      fail("scope.actions must list approved repair actions");
    } else {
      for (const action of scope.actions) {
        if (!allowedActions.has(action)) {
          fail(`scope.actions contains unsupported action: ${action}`);
        }
      }
    }
  }

  if (!Array.isArray(marker.conditions) || marker.conditions.length < 3) {
    fail("conditions must list the release constraints");
  }

  if (!Array.isArray(marker.evidence) || marker.evidence.length === 0) {
    fail("evidence must list repository evidence paths");
  } else {
    const evidence = new Set(marker.evidence);
    for (const path of requiredEvidence) {
      if (!evidence.has(path)) {
        fail(`evidence must include ${path}`);
      }
    }
    for (const path of marker.evidence) {
      if (!pathExists(path)) {
        fail(`evidence path is missing or not repository-relative: ${path}`);
      }
    }
  }

  if (marker.expires_at !== undefined) {
    if (!validIsoDate(marker.expires_at)) {
      fail("expires_at must be an ISO timestamp when present");
    } else if (Date.parse(marker.expires_at) <= Date.now()) {
      fail("expires_at must be in the future");
    }
  }

  return {
    ok: errors.length === 0,
    errors,
    acceptedScope: errors.length === 0 ? scope : null,
  };
}

function loadMarker(path) {
  const absolutePath = resolveMarkerPath(path);
  if (!existsSync(absolutePath)) {
    return {
      present: false,
      valid: false,
      errors: [`review marker is missing: ${publicMarkerSource(path)}`],
      acceptedScope: null,
    };
  }

  try {
    const marker = JSON.parse(readFileSync(absolutePath, "utf8"));
    const validation = validateMarker(marker);
    return {
      present: true,
      valid: validation.ok,
      errors: validation.errors,
      acceptedScope: validation.acceptedScope,
    };
  } catch (error) {
    return {
      present: true,
      valid: false,
      errors: [`review marker is not valid JSON: ${error.message}`],
      acceptedScope: null,
    };
  }
}

const options = parseArgs(process.argv.slice(2));
const marker = loadMarker(options.markerPath);

const blockers = marker.valid
  ? []
  : [
      ...marker.errors,
      `human release review must accept ${markerSchema} before delegated repair automation is enabled`,
    ];

const report = {
  kind: "covenant_a2a_repair_release_review",
  schema: reportSchema,
  generated_at: new Date().toISOString(),
  ready_for_delegated_repair_automation: marker.valid,
  status: marker.valid ? "review_accepted" : "human_required",
  human_decision_required: !marker.valid,
  review_marker: {
    schema: markerSchema,
    default_path: defaultMarker,
    source: publicMarkerSource(options.markerPath),
    present: marker.present,
    valid: marker.valid,
  },
  required_scope: {
    automation: "a2a.delegated-repair",
    actions: [...allowedActions],
    task_binding: "required",
    lease_binding: "required",
    counterparty_binding: "required",
    duplicate_risk_policy: "explicit-scope-only",
  },
  accepted_scope: marker.acceptedScope,
  evidence: [
    "docs/internal/decisions/0005-a2a-delegated-repair-release-review.md",
    "docs/a2a-repair-authorization.md",
    "docs/a2a-repair-visibility.md",
    "agent-os/scripts/a2a-repair-release-review.mjs",
    "agent-os/scripts/validate-a2a-repair-release-review.mjs",
  ],
  blockers,
  non_goals: [
    "granting repair capabilities",
    "starting delegated repair automation",
    "weakening task, lease, counterparty, or duplicate-risk scope checks",
  ],
};

if (options.asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`a2a delegated repair release review: ${report.status}`);
  console.log(`marker: ${report.review_marker.valid ? "valid" : report.review_marker.present ? "invalid" : "missing"}`);
  for (const blocker of blockers) {
    console.log(`blocker: ${blocker}`);
  }
}

if (options.strict && !marker.valid) {
  process.exit(1);
}
