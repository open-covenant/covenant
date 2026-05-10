#!/usr/bin/env node
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const script = join(repoRoot, "agent-os", "scripts", "identity-provenance.mjs");

function fail(message) {
  console.error(`validate-identity-provenance: ${message}`);
  process.exit(1);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

const home = mkdtempSync(join(tmpdir(), "covenant-identity-provenance-"));
try {
  mkdirSync(join(home, "identity"), { recursive: true });
  mkdirSync(join(home, "peers"), { recursive: true });
  const keyPath = join(home, "identity", "local.key");
  writeFileSync(keyPath, Buffer.alloc(32, 7));
  chmodSync(keyPath, 0o600);

  const tokenA = "2VzYqQ8mA11111111111111111111111111111111111";
  const tokenB = "9AbCdEfGh22222222222222222222222222222222222";
  const tokenC = "7LmNoPqRs33333333333333333333333333333333333";
  const redactedDisplay = "redacted-subject@local";
  const subject = "11111111111111111111111111111111";
  const other = "22222222222222222222222222222222";
  const events = [
    {
      type: "registered",
      token: tokenA,
      agent_id: { display: redactedDisplay, pubkey: subject },
      registered_at: 100,
    },
    { type: "revoked", token: tokenA, revoked_at: 200 },
    {
      type: "registered",
      token: tokenB,
      agent_id: { display: redactedDisplay, pubkey: subject },
      registered_at: 300,
    },
    {
      type: "registered",
      token: tokenC,
      agent_id: { display: "other@local", pubkey: other },
      registered_at: 400,
    },
  ];
  writeFileSync(
    join(home, "peers", "registry.jsonl"),
    `${events.map((event) => JSON.stringify(event)).join("\n")}\n`,
  );

  const result = spawnSync(process.execPath, [script, "--home", home, "--json"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert(result.status === 0, `script failed: ${result.stderr || result.stdout}`);
  assert(result.stderr.trim() === "", `script emitted stderr: ${result.stderr}`);

  const raw = result.stdout.trim();
  assert(!raw.includes(home), "report must not export the absolute Covenant home path");
  assert(!raw.includes(redactedDisplay), "report must redact peer display strings");
  for (const token of [tokenA, tokenB, tokenC]) {
    assert(!raw.includes(token), "report must not export full peer tokens");
  }

  const report = JSON.parse(raw);
  assert(report.schema === "covenant.identity-provenance.plan.v1", "schema mismatch");
  assert(report.mode === "dry_run", "report must be dry-run");
  assert(report.publish_supported === false, "public publishing must remain unsupported");
  assert(report.covenant_home.absolute_path_exported === false, "path export flag mismatch");
  assert(report.identity_key.exists === true, "identity key should be detected");
  assert(report.identity_key.byte_length === 32, "identity key size should be reported");
  assert(report.identity_key.seed_exported === false, "identity seed must not be exported");
  assert(report.peer_registry.token_secret_exported === false, "token export flag mismatch");
  assert(report.peer_registry.registered_count === 3, "registered peer count mismatch");
  assert(report.peer_registry.revoked_count === 1, "revoked peer count mismatch");
  assert(report.peer_registry.live_count === 2, "live peer count mismatch");
  assert(report.peer_registry.subjects.length === 2, "subject summary count mismatch");
  assert(
    report.peer_registry.subjects.some((entry) => entry.subject_pubkey_b58 === subject),
    "subject summary must include rotated subject",
  );
  assert(report.peer_registry.rows.every((row) => row.subject_display_redacted === true), "rows must redact displays");
  assert(report.peer_registry.rows.every((row) => row.full_token_exported === false), "rows must mark token redaction");

  const rotation = report.rotation_history.find((entry) => entry.subject_pubkey_b58 === subject);
  assert(rotation, "rotation history must include rotated subject");
  assert(rotation.registrations === 2, "rotation registration count mismatch");
  assert(rotation.live_tokens === 1, "rotation live-token count mismatch");
  assert(rotation.revoked_tokens === 1, "rotation revoked-token count mismatch");
  assert(report.blockers.some((blocker) => blocker.includes("human approval")), "human approval blocker missing");
} finally {
  rmSync(home, { recursive: true, force: true });
}

console.log("validate-identity-provenance: ok");
