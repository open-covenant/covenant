#!/usr/bin/env node
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const script = join(root, "scripts", "source-install-upgrade-plan.mjs");

function fail(message) {
  console.error(`validate-source-install-upgrade-plan: ${message}`);
  process.exit(1);
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

function run(prefix) {
  const result = spawnSync(process.execPath, [script, "--prefix", prefix, "--json"], {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });

  if (result.status !== 0) {
    fail(result.stderr || result.stdout || "upgrade preflight failed");
  }

  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    fail(`output is not JSON: ${error.message}`);
  }
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function writeFixtureInstall(prefix) {
  mkdirSync(join(prefix, "bin"), { recursive: true });
  mkdirSync(join(prefix, "share", "covenant"), { recursive: true });

  const daemon = "old covenantd fixture\n";
  const cli = "old covenant fixture\n";
  writeFileSync(join(prefix, "bin", "covenantd"), daemon);
  writeFileSync(join(prefix, "bin", "covenant"), cli);

  const manifest = {
    schema: "covenant.source-install.v1",
    generated_at: "2026-01-01T00:00:00.000Z",
    source_commit: "fixture-source",
    profile: "release",
    binaries: [
      {
        name: "covenantd",
        crate: "covenantd",
        source_target: "target/release/covenantd",
        installed_path: "bin/covenantd",
        bytes: Buffer.byteLength(daemon),
        sha256: sha256(daemon),
      },
      {
        name: "covenant",
        crate: "covenant",
        source_target: "target/release/covenant",
        installed_path: "bin/covenant",
        bytes: Buffer.byteLength(cli),
        sha256: sha256(cli),
      },
    ],
  };
  writeFileSync(
    join(prefix, "share", "covenant", "install-manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
}

const dir = mkdtempSync(join(tmpdir(), "covenant-source-upgrade-plan-"));

const fresh = run(join(dir, "fresh"));
assert(fresh.kind === "covenant_source_install_upgrade_plan", "unexpected report kind");
assert(fresh.schema === "covenant.source-install-upgrade-plan.v1", "unexpected report schema");
assert(fresh.upgrade_state === "fresh_install", "fresh prefix should be classified as fresh install");
assert(fresh.ready_for_fresh_install === true, "fresh install should be ready");
assert(fresh.ready_for_reviewed_source_upgrade === false, "fresh install is not an upgrade");
assert(fresh.ready_for_automatic_rollback === false, "automatic rollback must stay blocked");
assert(Array.isArray(fresh.planned_writes) && fresh.planned_writes.length === 3, "planned writes missing");

const cleanPrefix = join(dir, "clean");
writeFixtureInstall(cleanPrefix);
const clean = run(cleanPrefix);
assert(clean.upgrade_state === "clean_existing_install", "clean prefix should be classified as clean");
assert(clean.ready_for_reviewed_source_upgrade === true, "clean upgrade should be review-ready");
assert(clean.ready_for_automatic_rollback === false, "clean upgrade must not imply automatic rollback");
assert(clean.blockers.length === 0, "clean fixture should not report drift blockers");
for (const destination of clean.planned_writes.filter((write) => write.type === "binary")) {
  assert(destination.action === "replace", `${destination.destination} should be a replacement`);
  assert(destination.existing.matches_manifest === true, `${destination.destination} should match manifest`);
}

const driftPrefix = join(dir, "drift");
writeFixtureInstall(driftPrefix);
writeFileSync(join(driftPrefix, "bin", "covenantd"), "mutated daemon fixture\n");
const drift = run(driftPrefix);
assert(drift.upgrade_state === "drifted_existing_install", "drift prefix should be classified as drifted");
assert(drift.ready_for_reviewed_source_upgrade === false, "drifted install cannot be upgrade-ready");
assert(
  drift.blockers.some((blocker) => blocker.includes("existing binary does not match")),
  "drift blocker missing",
);

console.log("validate-source-install-upgrade-plan: ok");
