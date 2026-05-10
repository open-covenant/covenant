#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const schema = "covenant.a2a-peer-repair-report.v1";

function fail(message, code = 1) {
  console.error(`a2a-peer-repair-report: ${message}`);
  process.exit(code);
}

function parseArgs(argv) {
  const flags = {
    status: null,
    retry: null,
    nowMs: null,
    staleMs: null,
    json: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--status") {
      index += 1;
      flags.status = argv[index] || fail("--status requires a path", 2);
    } else if (arg === "--retry") {
      index += 1;
      flags.retry = argv[index] || fail("--retry requires a path", 2);
    } else if (arg === "--now-ms") {
      index += 1;
      flags.nowMs = parsePositiveInteger(argv[index], "--now-ms");
    } else if (arg === "--stale-ms") {
      index += 1;
      flags.staleMs = parsePositiveInteger(argv[index], "--stale-ms");
    } else if (arg === "--json") {
      flags.json = true;
    } else if (arg === "-h" || arg === "--help") {
      console.log("usage: node agent-os/scripts/a2a-peer-repair-report.mjs --status PATH [--retry PATH] [--now-ms N] [--stale-ms N] [--json]");
      process.exit(0);
    } else {
      fail(`unknown argument: ${arg}`, 2);
    }
  }

  if (!flags.status) {
    fail("--status is required", 2);
  }

  return flags;
}

function parsePositiveInteger(value, name) {
  const parsed = Number.parseInt(value ?? "", 10);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    fail(`${name} must be a non-negative integer`, 2);
  }
  return parsed;
}

function readJson(path, name) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`cannot read ${name} JSON: ${error.message}`);
  }
}

function pubkey(agent) {
  return typeof agent?.pubkey === "string" && agent.pubkey.trim() !== "" ? agent.pubkey : null;
}

function taskId(entry) {
  const id = entry?.task?.id;
  return typeof id === "string" && id.trim() !== "" ? id : null;
}

function stateOf(entry) {
  return typeof entry?.state === "string" ? entry.state : "unknown";
}

function peerForEntry(entry) {
  return pubkey(entry?.leased_to) || pubkey(entry?.task?.recipient) || "unattributed";
}

function sortedObject(counts) {
  return Object.fromEntries([...counts.entries()].sort(([a], [b]) => a.localeCompare(b)));
}

function emptyPeer(peer_pubkey) {
  return {
    peer_pubkey,
    queued_tasks: 0,
    in_flight_tasks: 0,
    stale_in_flight_tasks: 0,
    result_count: 0,
    retry_requeued: 0,
    retry_skipped: 0,
    skipped_reasons: {},
    tasks: [],
  };
}

function addSkipReason(peer, reason) {
  const counts = new Map(Object.entries(peer.skipped_reasons));
  counts.set(reason, (counts.get(reason) || 0) + 1);
  peer.skipped_reasons = sortedObject(counts);
}

function buildReport(flags) {
  const status = readJson(resolve(flags.status), "status");
  const retry = flags.retry ? readJson(resolve(flags.retry), "retry") : null;
  const tasks = Array.isArray(status.tasks) ? status.tasks : [];
  const results = Array.isArray(status.results) ? status.results : [];
  const staleMs = flags.staleMs ?? status.min_lease_age_ms ?? null;
  const peers = new Map();
  const taskPeers = new Map();
  let queuedTasks = 0;
  let inFlightTasks = 0;
  let staleInFlightTasks = 0;

  const ensurePeer = (peer) => {
    if (!peers.has(peer)) peers.set(peer, emptyPeer(peer));
    return peers.get(peer);
  };

  for (const entry of tasks) {
    const id = taskId(entry);
    const peerKey = peerForEntry(entry);
    const peer = ensurePeer(peerKey);
    if (id) taskPeers.set(id, peerKey);

    const state = stateOf(entry);
    const leaseAgeMs = flags.nowMs !== null && Number.isSafeInteger(entry.leased_at_ms)
      ? Math.max(0, flags.nowMs - entry.leased_at_ms)
      : null;
    const stale = state === "in_flight"
      && leaseAgeMs !== null
      && staleMs !== null
      && leaseAgeMs >= staleMs;

    if (state === "queued") {
      queuedTasks += 1;
      peer.queued_tasks += 1;
    } else if (state === "in_flight") {
      inFlightTasks += 1;
      peer.in_flight_tasks += 1;
      if (stale) {
        staleInFlightTasks += 1;
        peer.stale_in_flight_tasks += 1;
      }
    }

    peer.tasks.push({
      task_id: id,
      state,
      sender_pubkey: pubkey(entry?.task?.sender),
      recipient_pubkey: pubkey(entry?.task?.recipient),
      leased_to_pubkey: pubkey(entry?.leased_to),
      lease_id_present: typeof entry?.lease_id === "string",
      lease_age_ms: leaseAgeMs,
      stale,
      attempt: Number.isSafeInteger(entry?.attempt) ? entry.attempt : 0,
    });
  }

  for (const result of results) {
    const peerKey = taskPeers.get(result?.task_id);
    if (peerKey) ensurePeer(peerKey).result_count += 1;
  }

  const unattributedRetry = [];
  const retryReport = retry?.report ?? null;
  for (const row of retryReport?.requeued ?? []) {
    const peerKey = taskPeers.get(row.task_id);
    if (peerKey) {
      ensurePeer(peerKey).retry_requeued += 1;
    } else {
      unattributedRetry.push({ task_id: row.task_id ?? null, kind: "requeued" });
    }
  }
  for (const row of retryReport?.skipped ?? []) {
    const peerKey = taskPeers.get(row.task_id);
    if (peerKey) {
      const peer = ensurePeer(peerKey);
      peer.retry_skipped += 1;
      addSkipReason(peer, row.reason ?? "unknown");
    } else {
      unattributedRetry.push({
        task_id: row.task_id ?? null,
        kind: "skipped",
        reason: row.reason ?? "unknown",
      });
    }
  }

  const peerRows = [...peers.values()].sort((a, b) => a.peer_pubkey.localeCompare(b.peer_pubkey));
  const retryRequeued = peerRows.reduce((sum, peer) => sum + peer.retry_requeued, 0);
  const retrySkipped = peerRows.reduce((sum, peer) => sum + peer.retry_skipped, 0);

  return {
    kind: "covenant_a2a_peer_repair_report",
    schema,
    mode: "read_only",
    mutation_supported: false,
    source: {
      status_json_provided: true,
      retry_json_provided: Boolean(retry),
      paths_exported: false,
    },
    policy: {
      now_ms_provided: flags.nowMs !== null,
      stale_ms: staleMs,
    },
    summary: {
      peer_count: peerRows.length,
      task_count: tasks.length,
      queued_tasks: queuedTasks,
      in_flight_tasks: inFlightTasks,
      stale_in_flight_tasks: staleInFlightTasks,
      result_count: results.length,
      retry_requeued: retryRequeued,
      retry_skipped: retrySkipped,
      unattributed_retry_rows: unattributedRetry.length,
    },
    peers: peerRows,
    unattributed_retry: unattributedRetry,
    refusal: {
      repair_supported: false,
      reason: "peer repair reports are read-only; requeue and force-error remain explicit operator commands",
    },
  };
}

const flags = parseArgs(process.argv.slice(2));
const report = buildReport(flags);

if (flags.json) {
  console.log(JSON.stringify(report));
} else {
  console.log(`a2a peer repair report: ${report.summary.peer_count} peer(s), ${report.summary.stale_in_flight_tasks} stale in-flight task(s)`);
  console.log("mode: read_only");
  console.log("repair: unsupported");
}
