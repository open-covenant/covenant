#!/usr/bin/env node
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const script = join(repoRoot, "agent-os", "scripts", "a2a-peer-repair-report.mjs");

function fail(message) {
  console.error(`validate-a2a-peer-repair-report: ${message}`);
  process.exit(1);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function agent(display, pubkey) {
  return { display, pubkey };
}

const dir = mkdtempSync(join(tmpdir(), "covenant-a2a-peer-repair-"));
try {
  mkdirSync(dir, { recursive: true });
  const statusPath = join(dir, "status.json");
  const retryPath = join(dir, "retry.json");
  const peerA = "11111111111111111111111111111111";
  const peerB = "22222222222222222222222222222222";
  const sender = "33333333333333333333333333333333";
  const privateIntent = "private repair intent text";
  const status = {
    kind: "a2a_status",
    limit: 10,
    min_lease_age_ms: 5000,
    tasks: [
      {
        state: "queued",
        task: {
          id: "00000000-0000-0000-0000-000000000001",
          sender: agent("sender-display@local", sender),
          recipient: agent("peer-a-display@local", peerA),
          intent_text: privateIntent,
        },
        lease_id: null,
        leased_to: null,
        leased_at_ms: null,
        attempt: 0,
      },
      {
        state: "in_flight",
        task: {
          id: "00000000-0000-0000-0000-000000000002",
          sender: agent("sender-display@local", sender),
          recipient: agent("peer-a-display@local", peerA),
          intent_text: privateIntent,
        },
        lease_id: "00000000-0000-0000-0000-000000000102",
        leased_to: agent("peer-a-display@local", peerA),
        leased_at_ms: 1000,
        attempt: 2,
      },
      {
        state: "in_flight",
        task: {
          id: "00000000-0000-0000-0000-000000000003",
          sender: agent("sender-display@local", sender),
          recipient: agent("peer-b-display@local", peerB),
          intent_text: privateIntent,
        },
        lease_id: "00000000-0000-0000-0000-000000000103",
        leased_to: agent("peer-b-display@local", peerB),
        leased_at_ms: 9900,
        attempt: 1,
      },
    ],
    results: [{ task_id: "00000000-0000-0000-0000-000000000001", status: "ok", content: [] }],
  };
  const retry = {
    kind: "a2a_auto_retry",
    report: {
      requeued: [{ task_id: "00000000-0000-0000-0000-000000000002", lease_id: "00000000-0000-0000-0000-000000000102", attempt: 2 }],
      skipped: [
        { task_id: "00000000-0000-0000-0000-000000000003", reason: "unsafe_duplicate_safety", attempt: 1, lease_age_ms: 100 },
        { task_id: "00000000-0000-0000-0000-000000000999", reason: "missing_lease", attempt: 0 },
      ],
    },
  };
  writeFileSync(statusPath, JSON.stringify(status));
  writeFileSync(retryPath, JSON.stringify(retry));

  const result = spawnSync(
    process.execPath,
    [script, "--status", statusPath, "--retry", retryPath, "--now-ms", "10000", "--stale-ms", "5000", "--json"],
    { cwd: repoRoot, encoding: "utf8" },
  );

  assert(result.status === 0, `script failed: ${result.stderr || result.stdout}`);
  assert(result.stderr.trim() === "", `script emitted stderr: ${result.stderr}`);

  const raw = result.stdout.trim();
  assert(!raw.includes(dir), "report must not export local input paths");
  assert(!raw.includes("peer-a-display@local"), "report must not export peer display strings");
  assert(!raw.includes(privateIntent), "report must not export task intent text");

  const report = JSON.parse(raw);
  assert(report.kind === "covenant_a2a_peer_repair_report", "kind mismatch");
  assert(report.schema === "covenant.a2a-peer-repair-report.v1", "schema mismatch");
  assert(report.mode === "read_only", "mode mismatch");
  assert(report.mutation_supported === false, "report must be read-only");
  assert(report.source.paths_exported === false, "path export flag mismatch");
  assert(report.summary.peer_count === 2, "peer count mismatch");
  assert(report.summary.task_count === 3, "task count mismatch");
  assert(report.summary.queued_tasks === 1, "queued count mismatch");
  assert(report.summary.in_flight_tasks === 2, "in-flight count mismatch");
  assert(report.summary.stale_in_flight_tasks === 1, "stale count mismatch");
  assert(report.summary.result_count === 1, "result count mismatch");
  assert(report.summary.retry_requeued === 1, "retry requeued count mismatch");
  assert(report.summary.retry_skipped === 1, "retry skipped count mismatch");
  assert(report.summary.unattributed_retry_rows === 1, "unattributed retry count mismatch");

  const peerARow = report.peers.find((peer) => peer.peer_pubkey === peerA);
  const peerBRow = report.peers.find((peer) => peer.peer_pubkey === peerB);
  assert(peerARow, "peer A row missing");
  assert(peerBRow, "peer B row missing");
  assert(peerARow.queued_tasks === 1, "peer A queued count mismatch");
  assert(peerARow.in_flight_tasks === 1, "peer A in-flight count mismatch");
  assert(peerARow.stale_in_flight_tasks === 1, "peer A stale count mismatch");
  assert(peerARow.retry_requeued === 1, "peer A requeued count mismatch");
  assert(peerBRow.retry_skipped === 1, "peer B skipped count mismatch");
  assert(peerBRow.skipped_reasons.unsafe_duplicate_safety === 1, "peer B skipped reason mismatch");
  assert(report.refusal.repair_supported === false, "repair refusal missing");
} finally {
  rmSync(dir, { recursive: true, force: true });
}

console.log("validate-a2a-peer-repair-report: ok");
