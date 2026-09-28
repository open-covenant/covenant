#!/usr/bin/env node
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// docs/compute-network.md <-> code binding guard. The public network
// map cites tool names, HTTP routes, env knobs, and crate paths; all
// of them are re-extracted from the committed sources on every run so
// the doc can't drift silently in either direction:
//   - the doc's `compute.*` tool set must EQUAL the buyer crate's
//     `pub const *_TOOL` literals (the single source both demand
//     surfaces share) — a tool added to the code without a doc row, or
//     a doc row for a dropped tool, both fail;
//   - every route the doc names must exist in the coordinator's route
//     table;
//   - every COVENANT_* knob the doc names must be a quoted literal in
//     the crate sources (compute crates + covenantd, whose daemon
//     wiring owns the COVENANT_COMPUTE_ENABLED family);
//   - every relative markdown link must resolve to a committed file.

const here = dirname(fileURLToPath(import.meta.url));
const agentOsRoot = resolve(here, "..");
const repoRoot = resolve(agentOsRoot, "..");
const docPath = join(repoRoot, "docs", "compute-network.md");

const TOOL_CONST = /pub const [A-Z_]+_TOOL: &str = "(compute\.[a-z_]+)"/g;
const TOOL_MENTION = /compute\.[a-z_]+/g;
const ROUTE_LITERAL = /\.route\(\s*"(\/[^"]*)"/g;
const ROUTE_MENTION = /`(\/(?:federation|metrics|health)[a-z0-9/:_-]*)`/g;
const KNOB_LITERAL = /"(COVENANT_[A-Z0-9_]+)"/g;
const KNOB_MENTION = /COVENANT_[A-Z0-9_]+/g;
const LINK = /\]\(([^)]+\.md)\)/g;

function extract(regex, text) {
  return [...new Set([...text.matchAll(regex)].map((m) => m[1] ?? m[0]))].sort();
}

function rustFilesUnder(dir) {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...rustFilesUnder(path));
    } else if (entry.name.endsWith(".rs")) {
      files.push(path);
    }
  }
  return files.sort();
}

// Always-on self-test: prove every extractor extracts before trusting
// any comparison — a regex that silently matches nothing would make
// every direction vacuously green.
{
  const rs =
    'pub const SELFTEST_TOOL: &str = "compute.selftest";\n' +
    '.route("/federation/selftest", get(h))\n' +
    'std::env::var("COVENANT_COMPUTE_SELFTEST")';
  const md =
    "Call `compute.selftest` at `/federation/selftest` with " +
    "`COVENANT_COMPUTE_SELFTEST`; see [the readme](../README.md).";
  const checks = [
    [extract(TOOL_CONST, rs), "compute.selftest"],
    [extract(TOOL_MENTION, md), "compute.selftest"],
    [extract(ROUTE_LITERAL, rs), "/federation/selftest"],
    [extract(ROUTE_MENTION, md), "/federation/selftest"],
    [extract(KNOB_LITERAL, rs), "COVENANT_COMPUTE_SELFTEST"],
    [extract(KNOB_MENTION, md), "COVENANT_COMPUTE_SELFTEST"],
    [extract(LINK, md), "../README.md"],
  ];
  for (const [got, want] of checks) {
    if (got.join(",") !== want) {
      console.error(
        `validate-compute-network-doc: self-test failed — got [${got}], want [${want}]`,
      );
      process.exit(1);
    }
  }
}

const errors = [];
let doc;
try {
  doc = readFileSync(docPath, "utf8");
} catch (error) {
  console.error(`validate-compute-network-doc: cannot read ${docPath}: ${error.message}`);
  process.exit(1);
}

const buyerLib = readFileSync(
  join(agentOsRoot, "crates", "covenant-compute-buyer", "src", "lib.rs"),
  "utf8",
);
const codeTools = extract(TOOL_CONST, buyerLib);
const docTools = extract(TOOL_MENTION, doc);
for (const tool of codeTools) {
  if (!docTools.includes(tool)) {
    errors.push(`doc is missing tool ${tool} — the buyer crate exports it; add a row`);
  }
}
for (const tool of docTools) {
  if (!codeTools.includes(tool)) {
    errors.push(`doc names tool ${tool} — no such *_TOOL constant in the buyer crate`);
  }
}

const httpRs = readFileSync(
  join(agentOsRoot, "crates", "covenant-compute-coordinator", "src", "http.rs"),
  "utf8",
);
const codeRoutes = extract(ROUTE_LITERAL, httpRs);
for (const route of extract(ROUTE_MENTION, doc)) {
  if (!codeRoutes.includes(route)) {
    errors.push(`doc names route ${route} — not in the coordinator's route table`);
  }
}

const codeKnobs = new Set(
  [
    "covenant-compute-protocol",
    "covenant-compute-node",
    "covenant-compute-coordinator",
    "covenant-compute-buyer",
    "covenantd",
  ]
    .flatMap((crate) => rustFilesUnder(join(agentOsRoot, "crates", crate, "src")))
    .flatMap((path) => extract(KNOB_LITERAL, readFileSync(path, "utf8"))),
);
for (const knob of extract(KNOB_MENTION, doc)) {
  if (!codeKnobs.has(knob)) {
    errors.push(`doc names knob ${knob} — no crate source quotes it`);
  }
}

for (const link of extract(LINK, doc)) {
  if (!existsSync(resolve(join(repoRoot, "docs"), link))) {
    errors.push(`doc link ${link} does not resolve from docs/`);
  }
}

if (errors.length > 0) {
  console.error("validate-compute-network-doc: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-compute-network-doc: ok (${codeTools.length} tools bound, ` +
    `${extract(ROUTE_MENTION, doc).length} routes checked, ` +
    `${extract(KNOB_MENTION, doc).length} knobs checked, self-test passed)`,
);
