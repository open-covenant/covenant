#!/usr/bin/env node
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const statusPath = join(repoRoot, "docs", "internal", "status.md");

function usage() {
  console.log(`usage: validate-status-evidence

Validate path-like backticked evidence entries in docs/internal/status.md.`);
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

if (!existsSync(statusPath)) {
  console.log("validate-status-evidence: dormant (docs/internal/status.md absent)");
  process.exit(0);
}

const status = readFileSync(statusPath, "utf8").replace(/\r\n/g, "\n");
const errors = [];
const evidence = new Map();

function isPathLike(value) {
  if (value.trim() !== value || value === "") return false;
  if (/[\s`$]/.test(value)) return false;
  if (/^[a-z]+:\/\//i.test(value)) return false;
  if (value.startsWith("/")) return false;
  if (value.includes("*")) return false;
  if (value.includes("/")) return true;
  return /\.(md|mjs|js|ts|tsx|json|toml|rs|sh|yml|yaml|lock|txt|nix)$/i.test(value);
}

function record(path, line) {
  const lines = evidence.get(path) ?? [];
  lines.push(line);
  evidence.set(path, lines);
}

const lines = status.split("\n");
for (let index = 0; index < lines.length; index += 1) {
  const line = lines[index];
  for (const match of line.matchAll(/`([^`]+)`/g)) {
    const value = match[1];
    if (!isPathLike(value)) continue;
    record(value, index + 1);
  }
}

for (const [path, lineNumbers] of evidence) {
  if (path.startsWith("/") || path.includes("..") || path.includes("\\")) {
    errors.push(`${path}: evidence path must be repository-relative (lines ${lineNumbers.join(", ")})`);
    continue;
  }
  if (!existsSync(join(repoRoot, path))) {
    errors.push(`${path}: missing evidence path (lines ${lineNumbers.join(", ")})`);
  }
}

if (errors.length > 0) {
  console.error("validate-status-evidence: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(`validate-status-evidence: ok (${evidence.size} paths)`);
