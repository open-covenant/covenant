#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const schema = "covenant.settlement.receipt_migration.plan.v1";

function fail(message, code = 1) {
  console.error(`settlement-receipt-migration: ${message}`);
  process.exit(code);
}

function defaultReceiptsPath() {
  const home = process.env.COVENANT_HOME || join(process.env.HOME || "", ".covenant");
  return join(home, "receipts", "working.jsonl");
}

function parseArgs(argv) {
  const flags = {
    receipts: defaultReceiptsPath(),
    limit: 500,
    json: false,
    apply: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--receipts") {
      index += 1;
      flags.receipts = argv[index] || fail("--receipts requires a path", 2);
    } else if (arg === "--limit" || arg === "-n") {
      index += 1;
      const raw = argv[index] || fail("--limit requires a value", 2);
      flags.limit = Number.parseInt(raw, 10);
      if (!Number.isSafeInteger(flags.limit) || flags.limit < 1) {
        fail("--limit must be a positive integer", 2);
      }
    } else if (arg === "--json") {
      flags.json = true;
    } else if (arg === "--apply") {
      flags.apply = true;
    } else if (arg === "-h" || arg === "--help") {
      console.log("usage: node agent-os/scripts/settlement-receipt-migration.mjs [--receipts PATH] [--limit N] [--json]");
      process.exit(0);
    } else {
      fail(`unknown argument: ${arg}`, 2);
    }
  }

  if (flags.apply) {
    fail("apply is unsupported; settlement receipt migration is read-only", 2);
  }

  return flags;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isString(value) {
  return typeof value === "string" && value.trim() !== "";
}

function isNumber(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function validateReceipt(value) {
  const missing = [];
  if (!isObject(value)) missing.push("object");
  if (!isString(value?.id)) missing.push("id");
  if (!isString(value?.resource)) missing.push("resource");
  if (!isObject(value?.payer)) missing.push("payer");
  if (!isString(value?.payer?.pubkey)) missing.push("payer.pubkey");
  if (!isNumber(value?.credits_consumed)) missing.push("credits_consumed");
  if (!isNumber(value?.settled_at)) missing.push("settled_at");
  if (value?.memory_record_id !== undefined && value.memory_record_id !== null && !isString(value.memory_record_id)) {
    missing.push("memory_record_id");
  }

  return missing.length === 0 ? null : `invalid receipt shape: ${missing.join(", ")}`;
}

function readRows(path, limit) {
  if (!existsSync(path)) {
    return {
      exists: false,
      availableLineCount: 0,
      rows: [],
    };
  }

  const lines = readFileSync(path, "utf8")
    .split(/\r?\n/)
    .map((text, index) => ({ line: index + 1, text }))
    .filter((row) => row.text.trim() !== "");

  return {
    exists: true,
    availableLineCount: lines.length,
    rows: lines.slice(-limit),
  };
}

function receiptSummary(receipt, status) {
  return {
    receipt_id: receipt.id,
    payer_pubkey: receipt.payer.pubkey,
    resource: receipt.resource,
    credits_consumed: receipt.credits_consumed,
    settled_at: receipt.settled_at,
    batch_id: receipt.batch_id ?? null,
    onchain_settled: Boolean(receipt.tx_sig || receipt.onchain_sig),
    status,
  };
}

function buildReport(flags) {
  const path = resolve(flags.receipts);
  const input = readRows(path, flags.limit);
  const malformedRows = [];
  const receipts = [];

  for (const row of input.rows) {
    let parsed;
    try {
      parsed = JSON.parse(row.text);
    } catch (error) {
      malformedRows.push({
        line: row.line,
        category: "json_parse_error",
        error: error.message,
      });
      continue;
    }

    const validationError = validateReceipt(parsed);
    if (validationError) {
      malformedRows.push({
        line: row.line,
        category: "invalid_receipt_shape",
        error: validationError,
      });
      continue;
    }

    receipts.push(parsed);
  }

  const memoryReceipts = receipts.filter((receipt) => receipt.resource === "memory");
  const legacyMemoryReceipts = memoryReceipts.filter((receipt) => !receipt.memory_record_id);
  const correlatedMemoryReceipts = memoryReceipts.filter((receipt) => receipt.memory_record_id);
  const batchedReceiptCount = receipts.filter((receipt) => receipt.batch_id).length;

  return {
    schema,
    mode: "dry_run",
    mutation_supported: false,
    source: {
      kind: "settlement_receipt_jsonl",
      exists: input.exists,
      path_exported: false,
      limit: flags.limit,
      available_line_count: input.availableLineCount,
      scanned_line_count: input.rows.length,
    },
    summary: {
      parsed_receipt_count: receipts.length,
      malformed_row_count: malformedRows.length,
      memory_receipt_count: memoryReceipts.length,
      correlated_memory_receipt_count: correlatedMemoryReceipts.length,
      legacy_memory_receipt_count: legacyMemoryReceipts.length,
      non_memory_receipt_count: receipts.length - memoryReceipts.length,
      batched_receipt_count: batchedReceiptCount,
      unbatched_receipt_count: receipts.length - batchedReceiptCount,
    },
    expected_correlation_inputs: [
      "memory_record_id from the originating memory write",
      "payer pubkey match between receipt.payer and memory.owner",
      "reviewed before and after receipt hash evidence",
      "audit event id for any future authorized mutation",
      "rollback snapshot for the receipt JSONL before mutation",
    ],
    legacy_uncorrelated_receipts: legacyMemoryReceipts.map((receipt) =>
      receiptSummary(receipt, "needs_memory_record_match"),
    ),
    correlated_memory_receipts: correlatedMemoryReceipts.map((receipt) => ({
      ...receiptSummary(receipt, "already_correlated"),
      memory_record_id: receipt.memory_record_id,
    })),
    malformed_rows: malformedRows,
    refusal: {
      apply_supported: false,
      reason: "settlement receipt migration planning does not rewrite receipts; mutation requires a separate authorized command with rollback and audit evidence",
    },
    blockers:
      malformedRows.length === 0
        ? []
        : ["malformed receipt rows must be quarantined or manually repaired before any migration mutation is designed"],
  };
}

const flags = parseArgs(process.argv.slice(2));
const report = buildReport(flags);

if (flags.json) {
  console.log(JSON.stringify(report));
} else {
  const { summary } = report;
  console.log(`settlement receipt migration plan: ${summary.legacy_memory_receipt_count} legacy memory receipt(s), ${summary.malformed_row_count} malformed row(s)`);
  console.log("mode: dry_run");
  console.log("apply: unsupported");
}
