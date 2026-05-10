#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const matrixPath = "docs/internal/signed-capabilities-live-coverage.md";
const statusPath = "docs/internal/status.md";

if (!existsSync(join(repoRoot, matrixPath)) || !existsSync(join(repoRoot, statusPath))) {
  console.log("validate-signed-capabilities-live-coverage: dormant (internal docs absent)");
  process.exit(0);
}

let matrix;
let status;
try {
  matrix = read(matrixPath);
} catch (error) {
  console.error(`validate-signed-capabilities-live-coverage: cannot read ${matrixPath}: ${error.message}`);
  process.exit(1);
}
try {
  status = read(statusPath);
} catch (error) {
  console.error(`validate-signed-capabilities-live-coverage: cannot read ${statusPath}: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

const requiredHeadings = [
  "# Signed Capabilities Live Coverage Matrix",
  "## Coverage Marks",
  "## Matrix",
  "## Human Authority",
];
for (const heading of requiredHeadings) {
  if (!matrix.includes(heading)) {
    fail(`matrix missing section: ${heading}`);
  }
}

const requiredMarks = ["delegated-covered", "delegated-denial-only", "gap"];
for (const mark of requiredMarks) {
  if (!matrix.includes(mark)) {
    fail(`matrix missing coverage mark: ${mark}`);
  }
}

const requiredActions = [
  "`peers.revoke`",
  "`peers.list`",
  "`peers.purge`",
  "`a2a.send.<task_kind>`",
  "`a2a.recv.<task_kind>`",
  "`a2a.respond.<task_kind>`",
  "`a2a.repair.requeue`",
  "`tool.call.<name>`",
  "`audit.purge`",
  "`memory.read`, `memory.read.<tier>`",
  "`memory.write`",
  "`memory.purge`",
  "`memory.repair.*`",
  "`memory.compact.*`",
  "`chain.receipts`",
  "`chain.batches`",
  "`chain.flush`",
];
for (const action of requiredActions) {
  if (!matrix.includes(action)) {
    fail(`matrix missing action row: ${action}`);
  }
}

const matrixRows = matrix
  .split("\n")
  .filter((line) => /^\| `[^|]+` \| /.test(line));
if (matrixRows.length < requiredActions.length) {
  fail(`matrix must contain at least ${requiredActions.length} action rows, found ${matrixRows.length}`);
}

const livePathPattern = /agent-os\/crates\/[^`\s)]+\/tests\/live_[A-Za-z0-9_]+\.rs/g;
const cited = new Set();
let match;
while ((match = livePathPattern.exec(matrix)) !== null) {
  cited.add(match[0]);
}
if (cited.size === 0) {
  fail("matrix must cite at least one live test path");
}
for (const path of cited) {
  if (!existsSync(join(repoRoot, path))) {
    fail(`cited live test path does not exist: ${path}`);
  }
}

const peersRevokeRow = matrixRows.find((line) => line.includes("`peers.revoke`"));
if (peersRevokeRow && !peersRevokeRow.includes("live_peers_revoke.rs")) {
  fail("peers.revoke row must cite live_peers_revoke.rs");
}
if (peersRevokeRow && !peersRevokeRow.includes("delegated-covered")) {
  fail("peers.revoke row must mark delegated-covered");
}

const a2aRepairRow = matrixRows.find((line) => line.includes("`a2a.repair.requeue`"));
if (a2aRepairRow && !a2aRepairRow.includes("live_a2a.rs")) {
  fail("a2a.repair.requeue row must cite live_a2a.rs");
}
if (a2aRepairRow && !a2aRepairRow.includes("delegated-denial-only")) {
  fail("a2a.repair.requeue row must mark delegated-denial-only");
}

if (!matrix.includes("node agent-os/scripts/validate-signed-capabilities-live-coverage.mjs")) {
  fail("matrix must reference its own validator command");
}

const statusRow = status.split("\n").find((line) => /^\| Signed capabilities \|/.test(line));
if (!statusRow) {
  fail("status.md is missing the Signed capabilities row");
} else if (!statusRow.includes("docs/internal/signed-capabilities-live-coverage.md")) {
  fail("status.md Signed capabilities row must reference docs/internal/signed-capabilities-live-coverage.md");
}

const liveCoveragePath = "agent-os/autonomy/live-coverage.json";
let liveCoverage;
try {
  liveCoverage = JSON.parse(read(liveCoveragePath));
} catch (error) {
  fail(`cannot read ${liveCoveragePath}: ${error.message}`);
}

const delegatedScopeEntries = new Map();
if (liveCoverage && Array.isArray(liveCoverage.surfaces)) {
  for (const surface of liveCoverage.surfaces) {
    const blocks = Array.isArray(surface?.scopeCoverage) ? surface.scopeCoverage : [];
    for (const block of blocks) {
      if (block?.delegated === true && typeof block.namespace === "string") {
        delegatedScopeEntries.set(block.namespace, {
          surface: surface.id ?? null,
          liveTest: typeof block.liveTest === "string" ? block.liveTest : null,
          deniedEvidence: Array.isArray(block.deniedEvidence) ? block.deniedEvidence : [],
          allowedEvidence: Array.isArray(block.allowedEvidence) ? block.allowedEvidence : [],
        });
      }
    }
  }
}

function actionToNamespace(action) {
  let trimmed = action.trim();
  if (trimmed.startsWith("`") && trimmed.endsWith("`")) {
    trimmed = trimmed.slice(1, -1);
  }
  return trimmed;
}

function extractRowAction(row) {
  const match = row.match(/^\| (`[^|]+`) \| /);
  if (!match) {
    return null;
  }
  return actionToNamespace(match[1]);
}

const coveredMarks = new Set(["delegated-covered", "delegated-denial-only"]);
const matrixCovered = matrixRows
  .map((row) => {
    const action = extractRowAction(row);
    if (!action) return null;
    const cells = row.split("|").map((cell) => cell.trim());
    const liveCell = cells[3] ?? "";
    const coverageCell = cells[4] ?? "";
    return { action, liveCell, coverageCell };
  })
  .filter(Boolean)
  .filter((row) => coveredMarks.has(row.coverageCell));

for (const row of matrixCovered) {
  const namespaceCandidates = [row.action];
  if (row.action === "a2a.repair.requeue") {
    namespaceCandidates.push("a2a.repair.requeue");
  }
  let entry = null;
  for (const candidate of namespaceCandidates) {
    if (delegatedScopeEntries.has(candidate)) {
      entry = delegatedScopeEntries.get(candidate);
      break;
    }
  }
  if (!entry) {
    fail(
      `matrix row for ${row.action} (${row.coverageCell}) has no matching delegated:true scopeCoverage entry in ${liveCoveragePath}`,
    );
    continue;
  }
  if (entry.liveTest) {
    if (!row.liveCell.includes(entry.liveTest)) {
      fail(
        `matrix row for ${row.action} cites different live test than ${liveCoveragePath} (expected ${entry.liveTest})`,
      );
    }
  }
  if (row.coverageCell === "delegated-covered") {
    if (entry.allowedEvidence.length === 0) {
      fail(
        `matrix marks ${row.action} delegated-covered but ${liveCoveragePath} scopeCoverage has no allowedEvidence`,
      );
    }
  } else if (row.coverageCell === "delegated-denial-only") {
    if (entry.allowedEvidence.length > 0) {
      fail(
        `matrix marks ${row.action} delegated-denial-only but ${liveCoveragePath} scopeCoverage records allowedEvidence`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-signed-capabilities-live-coverage: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-signed-capabilities-live-coverage: ok (${matrixRows.length} action rows, ${cited.size} live test paths, ${matrixCovered.length} delegated bindings)`,
);
