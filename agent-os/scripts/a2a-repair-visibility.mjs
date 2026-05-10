#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(here));

function usage() {
  console.log(`usage: a2a-repair-visibility [--json] [--strict-delegated]

Report A2A repair visibility readiness without requeueing tasks, force-erroring
leases, starting daemons, or changing peer state.

Default mode exits 0 and reports blockers. Use --strict-delegated to exit non-zero
while delegated repair visibility gates are blocked.`);
}

function exists(path) {
  return existsSync(join(repoRoot, path));
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

function contains(path, text) {
  return exists(path) && read(path).includes(text);
}

const args = new Set(process.argv.slice(2));
if (args.has("--help") || args.has("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.has("--json");
const strictDelegated = args.has("--strict-delegated");
for (const arg of args) {
  if (!["--json", "--strict-delegated"].includes(arg)) {
    usage();
    process.exit(2);
  }
}

const queueDocsOk = contains("docs/a2a-queue-semantics.md", "Repair Contract")
  && contains("docs/a2a-queue-semantics.md", "peer_pubkey_b58")
  && contains("docs/a2a-queue-semantics.md", "duplicate_risk");
const idempotencyDocsOk = contains("docs/a2a-idempotency-policy.md", "Explicit retry gate")
  && contains("docs/a2a-idempotency-policy.md", "skipped tasks remain visible");
const cliRepairOk = contains("agent-os/crates/covenant/src/main.rs", "a2a requeue")
  && contains("agent-os/crates/covenant/src/main.rs", "a2a force-error")
  && contains("agent-os/crates/covenant/src/main.rs", "a2a retry-stale");
const liveRepairOk = exists("agent-os/crates/covenantd/tests/live_cli_a2a_repair.rs")
  && exists("agent-os/crates/covenantd/tests/live_cli_a2a_retry_stale_json.rs")
  && exists("agent-os/crates/covenantd/tests/live_restart_a2a.rs");
const perPeerRepairReportOk = exists("agent-os/scripts/a2a-peer-repair-report.mjs")
  && exists("agent-os/scripts/validate-a2a-peer-repair-report.mjs")
  && contains("docs/a2a-repair-visibility.md", "a2a-peer-repair-report.mjs")
  && contains("docs/a2a-repair-visibility.md", "covenant.a2a-peer-repair-report.v1");
const delegatedRepairPolicyOk = exists("docs/a2a-repair-authorization.md")
  && exists("agent-os/scripts/validate-a2a-repair-authorization.mjs")
  && contains("agent-os/crates/covenantd/src/lib.rs", "a2a_repair_rejects_peer_mismatched_delegated_scope");
const liveDelegatedRepairDenialOk = delegatedRepairPolicyOk
  && contains("agent-os/crates/covenantd/tests/live_a2a.rs", "live_covenantd_a2a_repair_rejects_peer_mismatched_delegation")
  && contains("agent-os/crates/covenantd/tests/live_a2a.rs", "CapabilityScopeRejected")
  && contains("agent-os/autonomy/live-coverage.json", "a2a.repair.requeue");

const gates = [
  {
    id: "operator-repair-contract",
    title: "Operator repair contract",
    status: queueDocsOk ? "documented" : "missing",
    ok: queueDocsOk,
    evidence: ["docs/a2a-queue-semantics.md"],
    blockers: queueDocsOk ? [] : ["A2A repair contract docs are missing peer and duplicate-risk visibility"],
    human_decision_required: false,
  },
  {
    id: "retry-visibility-contract",
    title: "Retry visibility contract",
    status: idempotencyDocsOk ? "documented" : "missing",
    ok: idempotencyDocsOk,
    evidence: ["docs/a2a-idempotency-policy.md"],
    blockers: idempotencyDocsOk ? [] : ["A2A retry policy docs do not state skipped-task visibility"],
    human_decision_required: false,
  },
  {
    id: "cli-repair-surfaces",
    title: "CLI repair surfaces",
    status: cliRepairOk ? "implemented" : "missing",
    ok: cliRepairOk,
    evidence: ["agent-os/crates/covenant/src/main.rs"],
    blockers: cliRepairOk ? [] : ["A2A repair CLI surfaces are missing"],
    human_decision_required: false,
  },
  {
    id: "live-operator-repair-coverage",
    title: "Live operator repair coverage",
    status: liveRepairOk ? "implemented" : "missing",
    ok: liveRepairOk,
    evidence: [
      "agent-os/crates/covenantd/tests/live_cli_a2a_repair.rs",
      "agent-os/crates/covenantd/tests/live_cli_a2a_retry_stale_json.rs",
      "agent-os/crates/covenantd/tests/live_restart_a2a.rs",
    ],
    blockers: liveRepairOk ? [] : ["operator repair live tests are missing"],
    human_decision_required: false,
  },
  {
    id: "per-peer-repair-report",
    title: "Per-peer repair report",
    status: perPeerRepairReportOk ? "implemented" : "missing",
    ok: perPeerRepairReportOk,
    evidence: [
      "agent-os/scripts/a2a-peer-repair-report.mjs",
      "agent-os/scripts/validate-a2a-peer-repair-report.mjs",
      "docs/a2a-repair-visibility.md",
    ],
    blockers: perPeerRepairReportOk
      ? []
      : [
          "repair reports do not yet group stale leases by peer pubkey",
          "retry-stale output does not yet summarize skipped unsafe tasks per peer",
          "operator views cannot yet compare delegated repair impact across peers",
        ],
    human_decision_required: false,
  },
  {
    id: "delegated-repair-denial-coverage",
    title: "Delegated repair denial coverage",
    status: liveDelegatedRepairDenialOk ? "implemented" : delegatedRepairPolicyOk ? "partial" : "planned",
    ok: liveDelegatedRepairDenialOk,
    evidence: liveDelegatedRepairDenialOk
      ? [
          "docs/a2a-repair-authorization.md",
          "agent-os/scripts/validate-a2a-repair-authorization.mjs",
          "agent-os/crates/covenantd/src/lib.rs",
          "agent-os/crates/covenantd/tests/live_a2a.rs",
          "agent-os/autonomy/live-coverage.json",
        ]
      : delegatedRepairPolicyOk
      ? [
          "docs/a2a-repair-authorization.md",
          "agent-os/scripts/validate-a2a-repair-authorization.mjs",
          "agent-os/crates/covenantd/src/lib.rs",
        ]
      : [],
    blockers: liveDelegatedRepairDenialOk
      ? []
      : delegatedRepairPolicyOk
      ? ["live peer-mismatched delegated repair coverage is still required before delegated repair expands"]
      : [
          "delegated repair expansion is not implemented",
          "tests do not yet prove a peer cannot repair another peer's leased task",
          "capability-scope denial fixtures for peer-mismatched repair are not present",
        ],
    human_decision_required: false,
  },
  {
    id: "delegated-repair-release-review",
    title: "Delegated repair release review",
    status: "human_required",
    ok: false,
    evidence: ["docs/a2a-repair-authorization.md", "docs/a2a-repair-visibility.md"],
    blockers: ["human review is required before delegated repair automation is enabled for cross-peer operators"],
    human_decision_required: true,
  },
];

const operatorGateIds = new Set([
  "operator-repair-contract",
  "retry-visibility-contract",
  "cli-repair-surfaces",
  "live-operator-repair-coverage",
]);
const operatorGates = gates.filter((gate) => operatorGateIds.has(gate.id));
const blockers = gates.filter((gate) => !gate.ok).map((gate) => gate.id);

const report = {
  kind: "covenant_a2a_repair_visibility",
  schema: "covenant.a2a-repair-visibility.v1",
  generated_at: new Date().toISOString(),
  ready_for_operator_repair_visibility: operatorGates.every((gate) => gate.ok),
  ready_for_delegated_repair: false,
  blockers,
  per_peer_repair_report: {
    schema: "covenant.a2a-peer-repair-report.v1",
    command: "node agent-os/scripts/a2a-peer-repair-report.mjs --status status.json --retry retry.json --json",
    validator: "node agent-os/scripts/validate-a2a-peer-repair-report.mjs",
  },
  delegated_repair_requirements: liveDelegatedRepairDenialOk
    ? ["human review before delegated repair automation"]
    : [
        "live peer-mismatched repair denial coverage",
        "human review before delegated repair automation",
      ],
  gates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`a2a repair visibility: ${report.ready_for_operator_repair_visibility ? "operator-ready" : "blocked"}`);
  console.log(`delegated repair: ${report.ready_for_delegated_repair ? "ready" : "blocked"}`);
  for (const gate of gates) {
    const marker = gate.ok ? "ok" : gate.status;
    console.log(`- ${marker}: ${gate.title}`);
    for (const blocker of gate.blockers) {
      console.log(`  blocker: ${blocker}`);
    }
  }
}

if (strictDelegated && !report.ready_for_delegated_repair) {
  process.exit(1);
}
