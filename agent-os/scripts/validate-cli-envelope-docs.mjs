#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const emittersPath = "agent-os/crates/covenant/src/main.rs";

const headings = ["## Chain Transaction Envelopes", "## CLI Read Envelopes"];

// Every CLI-emitted envelope discriminator (top-level `kind` for the unversioned
// subfamily, top-level `schema` for the versioned `covenant.<area>.<verb>.v<n>`
// subfamily) must appear in both the emitter (main.rs) and the docs section. If
// the validator fails for a new entry, either add the literal to the docs
// section or remove it from this array if the emitter renamed or dropped it.
// Scope is intentionally broader than the script's filename suggests; renaming
// to validate-cli-envelope-docs.mjs is a separate follow-up.
const kinds = [
  "covenant.chain.tx.v1",
  "covenant.chain.tx.timeout.v1",
  "covenant.settlement.backfill.v1",
  "covenant.memory.backfill.v1",
  "a2a_auto_retry",
  "a2a_compacted",
  "a2a_status",
  "audit_integrity",
  "audit_purged",
  "audit_recent",
  "bootstrap_result",
  "capabilities_purged",
  "capability_granted",
  "capability_list",
  "capability_revoked",
  "chain_status",
  "daemon_ping",
  "ignore_report",
  "intent_result",
  "intents_resume",
  "memory_compacted",
  "memory_compaction_plan",
  "memory_purged",
  "memory_read",
  "peer_list",
  "peer_revoke",
  "peer_token_rotated",
  "peers_purged",
  "receipt_batch_flushed",
  "receipt_batch_list",
  "receipt_list",
  "tool_list",
  "tool_result",
  "verify_report",
];

const errors = [];
const fail = (message) => errors.push(message);

let docs;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}

let emitters;
try {
  emitters = read(emittersPath);
} catch (error) {
  fail(`cannot read ${emittersPath}: ${error.message}`);
}

if (docs) {
  for (const heading of headings) {
    if (!docs.includes(heading)) {
      fail(
        `${docsPath} is missing the "${heading}" section; remediation: restore the section that pins the chain CLI envelopes`,
      );
    }
  }
}

if (docs) {
  for (const kind of kinds) {
    if (!docs.includes(kind)) {
      fail(
        `${docsPath} does not mention kind "${kind}"; remediation: the docs section must list every kind emitted by the chain CLI verbs`,
      );
    }
  }
}

if (emitters) {
  for (const kind of kinds) {
    if (!emitters.includes(kind)) {
      fail(
        `${emittersPath} does not emit kind "${kind}"; remediation: either the emitter was renamed and the docs are stale, or a verb was dropped — reconcile both sides`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-cli-envelope-docs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-cli-envelope-docs: ok (${kinds.length} kinds pinned across docs and emitters)`,
);
