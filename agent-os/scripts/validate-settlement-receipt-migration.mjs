#!/usr/bin/env node
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const script = join(repoRoot, "agent-os", "scripts", "settlement-receipt-migration.mjs");

function fail(message) {
  console.error(`validate-settlement-receipt-migration: ${message}`);
  process.exit(1);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

const dir = mkdtempSync(join(tmpdir(), "covenant-receipt-migration-"));
try {
  mkdirSync(join(dir, "receipts"), { recursive: true });
  const path = join(dir, "receipts", "working.jsonl");
  const legacyDisplay = "legacy-display@local";
  const correlatedDisplay = "correlated-display@local";
  const rawSecret = "raw-malformed-secret";
  const rows = [
    {
      id: "00000000-0000-0000-0000-000000000001",
      payer: { display: legacyDisplay, pubkey: "11111111111111111111111111111111" },
      resource: "memory",
      credits_consumed: 3,
      settled_at: 10,
    },
    {
      id: "00000000-0000-0000-0000-000000000002",
      payer: { display: correlatedDisplay, pubkey: "22222222222222222222222222222222" },
      resource: "memory",
      memory_record_id: "00000000-0000-0000-0000-000000000020",
      credits_consumed: 4,
      settled_at: 11,
      batch_id: "batch-a",
    },
    {
      id: "00000000-0000-0000-0000-000000000003",
      payer: { display: "compute-display@local", pubkey: "33333333333333333333333333333333" },
      resource: "compute",
      credits_consumed: 5,
      settled_at: 12,
    },
    `{ "id": "broken", "raw": "${rawSecret}"`,
    {
      id: "00000000-0000-0000-0000-000000000004",
      payer: { display: "missing-pubkey@local" },
      resource: "memory",
      credits_consumed: 1,
      settled_at: 13,
    },
  ];
  writeFileSync(
    path,
    `${rows.map((row) => (typeof row === "string" ? row : JSON.stringify(row))).join("\n")}\n`,
  );

  const result = spawnSync(process.execPath, [script, "--receipts", path, "--json"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert(result.status === 0, `script failed: ${result.stderr || result.stdout}`);
  assert(result.stderr.trim() === "", `script emitted stderr: ${result.stderr}`);

  const raw = result.stdout.trim();
  assert(!raw.includes(dir), "report must not export local receipt path");
  assert(!raw.includes(legacyDisplay), "report must not export payer display strings");
  assert(!raw.includes(correlatedDisplay), "report must not export correlated payer display strings");
  assert(!raw.includes(rawSecret), "report must not export malformed row contents");

  const report = JSON.parse(raw);
  assert(report.schema === "covenant.settlement.receipt_migration.plan.v1", "schema mismatch");
  assert(report.mode === "dry_run", "mode mismatch");
  assert(report.mutation_supported === false, "migration planner must remain read-only");
  assert(report.source.path_exported === false, "source path export flag mismatch");
  assert(report.source.scanned_line_count === 5, "scanned line count mismatch");
  assert(report.summary.parsed_receipt_count === 3, "parsed receipt count mismatch");
  assert(report.summary.malformed_row_count === 2, "malformed row count mismatch");
  assert(report.summary.memory_receipt_count === 2, "memory receipt count mismatch");
  assert(report.summary.legacy_memory_receipt_count === 1, "legacy memory count mismatch");
  assert(report.summary.correlated_memory_receipt_count === 1, "correlated memory count mismatch");
  assert(report.summary.non_memory_receipt_count === 1, "non-memory count mismatch");
  assert(report.summary.batched_receipt_count === 1, "batched receipt count mismatch");
  assert(report.legacy_uncorrelated_receipts.length === 1, "legacy rows mismatch");
  assert(report.correlated_memory_receipts.length === 1, "correlated rows mismatch");
  assert(report.malformed_rows.length === 2, "malformed row details mismatch");
  assert(
    report.expected_correlation_inputs.some((input) => input.includes("memory_record_id")),
    "expected correlation inputs must name memory_record_id",
  );
  assert(report.refusal.apply_supported === false, "apply refusal missing");
  assert(report.blockers.length === 1, "malformed rows should add one blocker");

  const apply = spawnSync(process.execPath, [script, "--receipts", path, "--apply"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert(apply.status !== 0, "apply must fail");
  assert(apply.stderr.includes("read-only"), "apply refusal should name read-only boundary");
} finally {
  rmSync(dir, { recursive: true, force: true });
}

console.log("validate-settlement-receipt-migration: ok");
