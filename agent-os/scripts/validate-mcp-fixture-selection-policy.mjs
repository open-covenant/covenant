#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const policyPath = "docs/mcp-fixture-selection-policy.md";
const statusPath = "docs/status.md";

let policy;
let status;
try {
  policy = read(policyPath);
} catch (error) {
  console.error(`validate-mcp-fixture-selection-policy: cannot read ${policyPath}: ${error.message}`);
  process.exit(1);
}
try {
  status = read(statusPath);
} catch (error) {
  console.error(`validate-mcp-fixture-selection-policy: cannot read ${statusPath}: ${error.message}`);
  process.exit(1);
}

const errors = [];
const fail = (message) => errors.push(message);

const requiredSections = [
  "# Third-Party MCP Fixture Selection Policy",
  "## Default-off Semantics",
  "## Provenance Pinning",
  "## License Check",
  "## Reproducible Build Evidence",
  "## Vendor Allowlist",
  "## Network Access",
  "## Removal Policy",
  "## Human Authority",
];
for (const heading of requiredSections) {
  if (!policy.includes(heading)) {
    fail(`policy missing section: ${heading}`);
  }
}

const requiredPhrases = [
  "must be opt-in",
  "specific upstream tag or commit hash",
  "MIT, Apache-2.0",
  "deterministic Cargo, npm, or shell build",
  "starts empty",
  "must not reach the public network",
  "removable in a single commit",
  "remain human-owned",
];
for (const phrase of requiredPhrases) {
  if (!policy.includes(phrase)) {
    fail(`policy missing required phrase: ${phrase}`);
  }
}

if (!/No fixture is approved by this document\./.test(policy)) {
  fail("policy must state that no fixture is approved by this document");
}

if (!policy.includes("node agent-os/scripts/validate-mcp-fixture-selection-policy.mjs")) {
  fail("policy must reference its own validator command");
}

const statusRow = status.split("\n").find((line) => /^\| MCP tools \|/.test(line));
if (!statusRow) {
  fail("status.md is missing the MCP tools row");
} else {
  if (!statusRow.includes("docs/mcp-fixture-selection-policy.md")) {
    fail("status.md MCP tools row must reference docs/mcp-fixture-selection-policy.md");
  }
  if (/opt-in third-party MCP server fixture (?:is|has) shipped/i.test(statusRow)) {
    fail("status.md MCP tools row must not claim a third-party fixture is shipping");
  }
}

if (errors.length > 0) {
  console.error("validate-mcp-fixture-selection-policy: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log("validate-mcp-fixture-selection-policy: ok");
