#!/usr/bin/env node
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const contractPath = "docs/ipc-and-http-gateway.md";
const statusPath = "docs/status.md";
const fixtureDirRel = "agent-os/crates/covenant-ipc/tests/fixtures/v2";
const migrationNoteRel = "docs/protocol-migrations/v2.md";

const errors = [];
const fail = (message) => errors.push(message);

let contract;
try {
  contract = read(contractPath);
} catch (error) {
  fail(`cannot read ${contractPath}: ${error.message}`);
}
let status;
try {
  status = read(statusPath);
} catch (error) {
  fail(`cannot read ${statusPath}: ${error.message}`);
}

if (contract) {
  for (const heading of [
    "# IPC and HTTP Gateway",
    "## v2 Fixture Contract",
    "### File Layout",
    "### Schema-version Field",
    "### Migration-note Pairing",
    "### Validator Behavior",
    "## Human Authority",
  ]) {
    if (!contract.includes(heading)) {
      fail(`contract missing section: ${heading}`);
    }
  }
  if (!contract.includes("node agent-os/scripts/validate-ipc-http-gateway-v2-fixture-contract.mjs")) {
    fail("contract must reference its own validator command");
  }
  if (!contract.includes("`*.v2.json`")) {
    fail("contract must name the *.v2.json filename convention");
  }
  if (!contract.includes(`agent-os/crates/covenant-ipc/tests/fixtures/v2/`)) {
    fail("contract must name the v2 staging directory");
  }
  if (!contract.includes("docs/protocol-migrations/v2.md")) {
    fail("contract must name the v2 migration note path");
  }
  if (!/remains? human-owned/.test(contract)) {
    fail("contract must state that the v2 protocol bump remains human-owned");
  }
}

if (status) {
  const statusRow = status.split("\n").find((line) => /^\| IPC and HTTP gateway \|/.test(line));
  if (!statusRow) {
    fail("status.md is missing the IPC and HTTP gateway row");
  } else if (!statusRow.includes("docs/ipc-and-http-gateway.md")) {
    fail("status.md IPC and HTTP gateway row must reference docs/ipc-and-http-gateway.md");
  }
}

const fixtureDir = join(repoRoot, fixtureDirRel);
const fixtureFiles = [];
if (existsSync(fixtureDir) && statSync(fixtureDir).isDirectory()) {
  for (const entry of readdirSync(fixtureDir)) {
    if (/\.v2\.json$/.test(entry)) {
      fixtureFiles.push(entry);
    }
  }
}

if (fixtureFiles.length === 0) {
  if (errors.length > 0) {
    console.error("validate-ipc-http-gateway-v2-fixture-contract: failed");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    process.exit(1);
  }
  console.log("validate-ipc-http-gateway-v2-fixture-contract: dormant (no v2 fixtures present)");
  process.exit(0);
}

const migrationPath = join(repoRoot, migrationNoteRel);
let migrationNote = null;
if (existsSync(migrationPath)) {
  try {
    migrationNote = readFileSync(migrationPath, "utf8");
  } catch (error) {
    fail(`cannot read ${migrationNoteRel}: ${error.message}`);
  }
} else {
  fail(`migration note ${migrationNoteRel} is missing; remediation: create the v2 migration note before adding v2 fixtures`);
}

for (const file of fixtureFiles) {
  const path = join(fixtureDir, file);
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`v2 fixture ${file} is not valid JSON: ${error.message}`);
    continue;
  }

  function findVersionTwo(value) {
    if (Array.isArray(value)) {
      return value.some(findVersionTwo);
    }
    if (value && typeof value === "object") {
      if (Object.prototype.hasOwnProperty.call(value, "version") && value.version === 2) {
        return true;
      }
      return Object.values(value).some(findVersionTwo);
    }
    return false;
  }

  if (!findVersionTwo(parsed)) {
    fail(
      `v2 fixture ${file} does not declare "version": 2; remediation: set the wire payload version field to 2 to match the file suffix`,
    );
  }

  if (migrationNote && !migrationNote.includes(file)) {
    fail(
      `v2 fixture ${file} is not referenced in ${migrationNoteRel}; remediation: add the fixture filename to the v2 migration note`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-ipc-http-gateway-v2-fixture-contract: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ipc-http-gateway-v2-fixture-contract: ok (${fixtureFiles.length} v2 fixture${fixtureFiles.length === 1 ? "" : "s"})`,
);
