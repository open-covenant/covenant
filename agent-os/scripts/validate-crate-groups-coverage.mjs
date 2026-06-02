#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");
const cargoPath = join(agentOsRoot, "Cargo.toml");
const readmePath = join(agentOsRoot, "README.md");

function usage() {
  console.log(`usage: validate-crate-groups-coverage

Assert every crates/covenant-* workspace member in agent-os/Cargo.toml is
backticked under the '## Crate Groups' table in agent-os/README.md, and that
the table lists no covenant-* crate that is not a workspace member.`);
}

const args = process.argv.slice(2);
if (args.includes("--help") || args.includes("-h")) {
  usage();
  process.exit(0);
}
if (args.length > 0) {
  usage();
  process.exit(2);
}

const cargo = readFileSync(cargoPath, "utf8").replace(/\r\n/g, "\n");
const membersStart = cargo.indexOf("members = [");
const membersEnd = membersStart === -1 ? -1 : cargo.indexOf("]", membersStart);
if (membersStart === -1 || membersEnd === -1) {
  console.error("validate-crate-groups-coverage: failed");
  console.error("- could not locate the members = [...] array in agent-os/Cargo.toml");
  process.exit(1);
}

const members = [...cargo.slice(membersStart, membersEnd).matchAll(/"([^"]+)"/g)].map((m) => m[1]);
const required = members
  .filter((path) => path.startsWith("crates/covenant-"))
  .map((path) => path.slice("crates/".length));

if (required.length === 0) {
  console.error("validate-crate-groups-coverage: failed");
  console.error("- no crates/covenant-* members parsed from agent-os/Cargo.toml");
  process.exit(1);
}

const readme = readFileSync(readmePath, "utf8").replace(/\r\n/g, "\n");
const lines = readme.split("\n");
const headingIndex = lines.findIndex((line) => line.trim() === "## Crate Groups");
if (headingIndex === -1) {
  console.error("validate-crate-groups-coverage: failed");
  console.error("- '## Crate Groups' section not found in agent-os/README.md");
  process.exit(1);
}

let sectionEnd = lines.length;
for (let index = headingIndex + 1; index < lines.length; index += 1) {
  if (/^## /.test(lines[index])) {
    sectionEnd = index;
    break;
  }
}

const tokens = new Set();
for (let index = headingIndex + 1; index < sectionEnd; index += 1) {
  for (const match of lines[index].matchAll(/`([^`]+)`/g)) {
    tokens.add(match[1]);
  }
}

const errors = [];
for (const crate of required) {
  if (!tokens.has(crate)) {
    errors.push(`${crate}: workspace member missing from the Crate Groups table`);
  }
}

const requiredSet = new Set(required);
for (const token of tokens) {
  if (token.startsWith("covenant-") && !requiredSet.has(token)) {
    errors.push(`${token}: documented in the Crate Groups table but not a crates/covenant-* workspace member`);
  }
}

if (errors.length > 0) {
  console.error("validate-crate-groups-coverage: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(`validate-crate-groups-coverage: ok (${required.length} covenant-* crates)`);
