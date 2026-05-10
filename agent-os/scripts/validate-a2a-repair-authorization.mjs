#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function fail(message) {
  console.error(`validate-a2a-repair-authorization: ${message}`);
  process.exit(1);
}

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8");
}

function requireIncludes(path, needles) {
  const source = read(path);
  for (const needle of needles) {
    if (!source.includes(needle)) {
      fail(`${path} is missing ${JSON.stringify(needle)}`);
    }
  }
  return source;
}

const docs = requireIncludes("docs/a2a-repair-authorization.md", [
  "a2a.repair.requeue",
  "a2a.repair.force_error",
  "peer_pubkey_b58",
  "task_id",
  "lease_id",
  "duplicate_risk",
  "counterparty",
  "capability_scope_rejected",
  "Delegated repair is still not release-ready",
  "covenant.a2a-delegated-repair-release-review.v1",
  "a2a-repair-release-review.mjs --strict",
]);

const daemon = requireIncludes("agent-os/crates/covenantd/src/lib.rs", [
  "a2a_repair_rejects_peer_mismatched_delegated_scope",
  "delegate retry probe",
  "capability scope",
  "CapabilityScopeRejected",
]);

requireIncludes("docs/a2a-repair-visibility.md", [
  "a2a-repair-authorization.md",
  "delegated authorization policy",
  "a2a-repair-release-review.mjs",
  "covenant.a2a-delegated-repair-release-review.v1",
]);

requireIncludes("docs/decisions/0005-a2a-delegated-repair-release-review.md", [
  "covenant.a2a-delegated-repair-release-review.v1",
  "node agent-os/scripts/a2a-repair-release-review.mjs --strict",
  "counterparty_binding",
  "duplicate_risk_policy",
]);

const home = process.env.HOME;
if (home && (docs.includes(home) || daemon.includes(home))) {
  fail("authorization evidence must not contain absolute home paths");
}

console.log("validate-a2a-repair-authorization: ok");
