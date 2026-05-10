#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const statusPath = join(repoRoot, "docs", "status.md");

function usage() {
  console.log(`usage: autonomy-status-gaps [--json]

Report autonomous task candidates from docs/status.md without mutating files or Git metadata.`);
}

function fail(message, code = 1) {
  console.error(message);
  process.exit(code);
}

function splitTableRow(line) {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function slug(value) {
  const cleaned = value
    .replace(/`[^`]+`/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return cleaned || "task";
}

function shortSlug(value, limit = 72) {
  const parts = slug(value).split("-").filter(Boolean);
  const selected = [];
  for (const part of parts) {
    const next = [...selected, part].join("-");
    if (next.length > limit) break;
    selected.push(part);
  }
  return selected.join("-") || "task";
}

function classify(status) {
  const lower = status.toLowerCase();
  if (lower.includes("planned")) return "planned";
  if (lower.includes("experimental")) return "experimental";
  if (lower.includes("partial")) return "partial";
  if (lower.includes("hardening")) return "hardening";
  return "implemented-hardening";
}

function priorityFor(category) {
  if (["planned", "experimental", "partial", "hardening"].includes(category)) {
    return "high";
  }
  return "medium";
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}

const asJson = args.includes("--json");
for (const arg of args) {
  if (arg !== "--json") {
    usage();
    process.exit(2);
  }
}

let status;
try {
  status = readFileSync(statusPath, "utf8").replace(/\r\n/g, "\n");
} catch (error) {
  fail(`autonomy-status-gaps: cannot read docs/status.md: ${error.message}`);
}

const candidates = [];
for (const line of status.split("\n")) {
  if (!line.startsWith("|")) continue;

  const cells = splitTableRow(line);
  if (cells.length !== 4) continue;

  const [capability, statusValue, evidence, nextHardeningStep] = cells;
  if (capability === "Capability" || /^-+$/.test(capability)) continue;
  if (!capability || !statusValue || !nextHardeningStep) continue;

  const category = classify(statusValue);
  const id = `status-${shortSlug(capability, 32)}-${shortSlug(nextHardeningStep, 44)}`;
  candidates.push({
    capability,
    status: statusValue,
    category,
    recommended_priority: priorityFor(category),
    suggested_task_id: id,
    suggested_title: nextHardeningStep.replace(/\.$/, ""),
    next_hardening_step: nextHardeningStep,
    evidence,
  });
}

if (candidates.length === 0) {
  fail("autonomy-status-gaps: no capability rows found in docs/status.md");
}

const categoryRank = new Map([
  ["planned", 100],
  ["partial", 90],
  ["experimental", 80],
  ["hardening", 70],
  ["implemented-hardening", 60],
]);

candidates.sort((left, right) => {
  const byCategory = (categoryRank.get(right.category) ?? 0) - (categoryRank.get(left.category) ?? 0);
  if (byCategory !== 0) return byCategory;
  return left.capability.localeCompare(right.capability);
});

const report = {
  kind: "autonomy_status_gaps",
  generated_at: new Date().toISOString(),
  source: relative(repoRoot, statusPath),
  candidate_count: candidates.length,
  candidates,
};

if (asJson) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`autonomy status gaps: ${candidates.length} candidate(s) from ${report.source}`);
  for (const candidate of candidates) {
    console.log(`- ${candidate.recommended_priority}: ${candidate.capability} (${candidate.status})`);
    console.log(`  ${candidate.next_hardening_step}`);
    console.log(`  suggested id: ${candidate.suggested_task_id}`);
  }
}
