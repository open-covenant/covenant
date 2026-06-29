#!/usr/bin/env node
// scripts/metrics.mjs — regenerate the README status line from real source.
//
// Default: verify the README's <!-- METRICS:START -->...<!-- METRICS:END -->
// block matches what current source produces; exit 1 on drift so the
// validate.sh gate catches stale numbers.
// Pass --write to rewrite the block in place.
//
// Counts mirror the public conventions:
//   * crates       — agent-os/crates/<name>/Cargo.toml with [package]
//   * Rust lines   — non-blank lines under agent-os/{crates,agents,programs}
//                    *.rs files, excluding target/
//   * tests        — #[test] / #[tokio::test] functions (same state machine
//                    as scripts/test-stats.sh, so the two surfaces agree)
//   * live tests   — of the above, those whose fn name starts with `live_`

import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const AGENT_OS = join(SCRIPT_DIR, "..");
const REPO = join(AGENT_OS, "..");
const README = join(REPO, "README.md");

const START = "<!-- METRICS:START -->";
const END = "<!-- METRICS:END -->";

function* walkRustSources(root) {
  let entries;
  try {
    entries = readdirSync(root, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    if (entry.name === "target" || entry.name === "node_modules") continue;
    const path = join(root, entry.name);
    if (entry.isDirectory()) {
      yield* walkRustSources(path);
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      yield path;
    }
  }
}

function countCrates() {
  const cratesDir = join(AGENT_OS, "crates");
  let count = 0;
  for (const entry of readdirSync(cratesDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = join(cratesDir, entry.name, "Cargo.toml");
    try {
      const text = readFileSync(manifest, "utf8");
      if (/^\s*\[package\]/m.test(text)) count++;
    } catch {
      // missing or unreadable manifest — not a crate
    }
  }
  return count;
}

function scanRust() {
  let lines = 0;
  const tests = new Set();
  for (const dirName of ["crates", "agents", "programs"]) {
    const root = join(AGENT_OS, dirName);
    for (const file of walkRustSources(root)) {
      const src = readFileSync(file, "utf8").split("\n");
      let armed = false;
      for (const raw of src) {
        if (raw.trim() !== "") lines++;
        if (/#\[(tokio::)?test\]/.test(raw)) {
          armed = true;
          continue;
        }
        if (!armed) continue;
        const fnMatch = raw.match(/fn\s+([A-Za-z_][A-Za-z0-9_]*)/);
        if (fnMatch) {
          tests.add(fnMatch[1]);
          armed = false;
        } else if (!/^\s*#/.test(raw)) {
          armed = false;
        }
      }
    }
  }
  let live = 0;
  for (const name of tests) if (name.startsWith("live_")) live++;
  return { lines, tests: tests.size, live };
}

function formatLines(n) {
  return n >= 1000 ? `~${Math.round(n / 1000)}k lines` : `${n} lines`;
}

function renderBlock({ crates, lines, tests, live }) {
  return `${START}
**Status.** Local control plane is real and live-tested (${crates} Rust crates, ${formatLines(lines)}, ${tests} source-discovered Rust tests including ${live} live boundary tests). Production-grade sandboxing for hostile agent code and networked multi-peer operation are roadmap; the Solana settlement program is deployed on mainnet (credits, staking, slashing, on-chain receipt anchoring), but its daemon-driven economic lifecycle is not yet production. See [BUILT.md](./BUILT.md) for the explicit honesty boundary.
${END}`;
}

function locateBlock(text) {
  const start = text.indexOf(START);
  const end = text.indexOf(END);
  if (start < 0 || end < 0 || end < start) return null;
  return { start, end: end + END.length };
}

const write = process.argv.includes("--write");
const text = readFileSync(README, "utf8");
const range = locateBlock(text);
if (!range) {
  console.error("metrics: README is missing the METRICS:{START,END} markers");
  process.exit(1);
}

const counts = { crates: countCrates(), ...scanRust() };
const rendered = renderBlock(counts);
const next = text.slice(0, range.start) + rendered + text.slice(range.end);

const summary = `crates=${counts.crates} lines=${counts.lines} tests=${counts.tests} live=${counts.live}`;

if (next === text) {
  console.log(`metrics: ok (${summary})`);
  process.exit(0);
}

if (write) {
  writeFileSync(README, next);
  console.log(`metrics: wrote README (${summary})`);
  process.exit(0);
}

console.error("metrics: README status block is stale");
console.error(`expected: ${summary}`);
console.error("run: node agent-os/scripts/metrics.mjs --write");
process.exit(1);
