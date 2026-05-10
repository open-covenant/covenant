#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

function fail(message, code = 1) {
  console.error(`identity-provenance: ${message}`);
  process.exit(code);
}

function parseArgs(argv) {
  const flags = {
    home: process.env.COVENANT_HOME || join(process.env.HOME || "", ".covenant"),
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--home") {
      i += 1;
      flags.home = argv[i] || fail("--home requires a path", 2);
    } else if (arg === "--json") {
      flags.json = true;
    } else if (arg === "-h" || arg === "--help") {
      console.log("usage: node agent-os/scripts/identity-provenance.mjs [--home PATH] [--json]");
      process.exit(0);
    } else {
      fail(`unknown argument: ${arg}`, 2);
    }
  }
  return flags;
}

function hash(value) {
  return createHash("sha256").update(String(value)).digest("hex");
}

function shortHash(value) {
  return hash(value).slice(0, 16);
}

function tokenPrefix(token) {
  return typeof token === "string" ? token.slice(0, 6) : null;
}

function fileState(path) {
  if (!existsSync(path)) {
    return {
      exists: false,
      byteLength: 0,
      mode: null,
    };
  }
  const stat = statSync(path);
  return {
    exists: true,
    byteLength: stat.size,
    mode: (stat.mode & 0o777).toString(8).padStart(3, "0"),
  };
}

function readPeerEvents(path) {
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim() !== "")
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (error) {
        fail(`${path}:${index + 1}: ${error.message}`);
      }
    });
}

function peerRows(events) {
  const revoked = new Map();
  for (const event of events) {
    if (event.type === "revoked" && typeof event.token === "string") {
      revoked.set(event.token, event.revoked_at ?? null);
    }
  }

  return events
    .filter((event) => event.type === "registered")
    .map((event) => {
      const pubkey = event.agent_id?.pubkey ?? null;
      const display = event.agent_id?.display ?? "";
      const revokedAt = revoked.get(event.token) ?? null;
      return {
        subject_pubkey_b58: pubkey,
        subject_display_hash: shortHash(display),
        subject_display_redacted: true,
        token_prefix: tokenPrefix(event.token),
        full_token_exported: false,
        registered_at: event.registered_at ?? null,
        revoked_at: revokedAt,
        status: revokedAt === null ? "live" : "revoked",
      };
    });
}

function rotationHistory(rows) {
  const bySubject = new Map();
  for (const row of rows) {
    if (!row.subject_pubkey_b58) continue;
    const current = bySubject.get(row.subject_pubkey_b58) || [];
    current.push(row);
    bySubject.set(row.subject_pubkey_b58, current);
  }

  return [...bySubject.entries()]
    .filter(([, subjectRows]) => subjectRows.length > 1 || subjectRows.some((row) => row.revoked_at))
    .map(([subject, subjectRows]) => {
      const registrations = subjectRows
        .map((row) => row.registered_at)
        .filter((value) => typeof value === "number");
      return {
        subject_pubkey_b58: subject,
        registrations: subjectRows.length,
        live_tokens: subjectRows.filter((row) => row.status === "live").length,
        revoked_tokens: subjectRows.filter((row) => row.status === "revoked").length,
        first_registered_at: registrations.length ? Math.min(...registrations) : null,
        last_registered_at: registrations.length ? Math.max(...registrations) : null,
        live_token_prefixes: subjectRows
          .filter((row) => row.status === "live")
          .map((row) => row.token_prefix),
        revoked_token_prefixes: subjectRows
          .filter((row) => row.status === "revoked")
          .map((row) => row.token_prefix),
      };
    });
}

function subjects(rows) {
  const bySubject = new Map();
  for (const row of rows) {
    if (!row.subject_pubkey_b58) continue;
    const subject = bySubject.get(row.subject_pubkey_b58) || {
      subject_pubkey_b58: row.subject_pubkey_b58,
      display_hashes: new Set(),
      registrations: 0,
      live_tokens: 0,
      revoked_tokens: 0,
    };
    subject.display_hashes.add(row.subject_display_hash);
    subject.registrations += 1;
    if (row.status === "live") {
      subject.live_tokens += 1;
    } else {
      subject.revoked_tokens += 1;
    }
    bySubject.set(row.subject_pubkey_b58, subject);
  }

  return [...bySubject.values()].map((subject) => ({
    ...subject,
    display_hashes: [...subject.display_hashes].sort(),
  }));
}

function buildReport(flags) {
  const home = resolve(flags.home);
  const identityPath = join(home, "identity", "local.key");
  const registryPath = join(home, "peers", "registry.jsonl");
  const identityKey = fileState(identityPath);
  const events = readPeerEvents(registryPath);
  const rows = peerRows(events);

  return {
    schema: "covenant.identity-provenance.plan.v1",
    mode: "dry_run",
    publish_supported: false,
    covenant_home: {
      source: flags.home === process.env.COVENANT_HOME ? "COVENANT_HOME" : "argument_or_default",
      absolute_path_exported: false,
    },
    identity_key: {
      exists: identityKey.exists,
      byte_length: identityKey.byteLength,
      mode: identityKey.mode,
      seed_exported: false,
      expected_seed_bytes: 32,
      expected_mode: "600",
    },
    peer_registry: {
      exists: existsSync(registryPath),
      event_count: events.length,
      registered_count: rows.length,
      live_count: rows.filter((row) => row.status === "live").length,
      revoked_count: rows.filter((row) => row.status === "revoked").length,
      token_secret_exported: false,
      subjects: subjects(rows),
      rows,
    },
    rotation_history: rotationHistory(rows),
    blockers: [
      "human approval is required before publishing public identity attestations",
      "project key custody and publication location are not yet approved",
      "local peer tokens remain secrets and are never exported by this report",
    ],
  };
}

const flags = parseArgs(process.argv.slice(2));
const report = buildReport(flags);

if (flags.json) {
  console.log(JSON.stringify(report));
} else {
  console.log(`identity provenance plan: ${report.peer_registry.registered_count} registered peer row(s), ${report.rotation_history.length} rotation subject(s)`);
  console.log("mode: dry_run");
  console.log("publish: unsupported");
}
