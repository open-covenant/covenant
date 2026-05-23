#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_read envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites four inner assertion ranges inside
// memory_read_json_pins_top_level_schema:
//
//   - line 413 cites `tier` (string or null) at main.rs:6838-6841.
//   - line 414 cites `limit` (u64) at main.rs:6834-6837.
//   - line 415 cites `query` (string or null) at main.rs:6842-6845.
//   - line 416 cites `min_relevance` (f64 or null) at main.rs:6846-6849.
//
// All docs cites use the assert!-to-closing convention: the cite spans
// from the line containing `assert!(` through the closing `);` line.
// This convention is chosen to match the existing docs cites without
// requiring a docs prose change.
//
// The validator scopes each selector lookup to the brace-balanced
// memory_read_json_pins_top_level_schema fn body so a same-named
// selector inside a different envelope's pins test cannot contaminate
// the result.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "memory_read_json_pins_top_level_schema";

const targets = [
  {
    field: "tier",
    selector: 'value["tier"].is_string() || value["tier"].is_null(),',
    docsRegex:
      /Pinned as string-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never a structured object\./,
    docsLabel: "memory_read.tier type-level pin citation",
    docsTemplate:
      "Pinned as string-or-null by the schema test (`main.rs:N-M`) — never a structured object.",
  },
  {
    field: "query",
    selector: 'value["query"].is_string() || value["query"].is_null(),',
    docsRegex:
      /- `query` \(string or null\): for `mode="search"`, the request query \(whitespace-joined when the operator passed multiple positional tokens, per `main\.rs:\d+`\)\. For `mode="recent"`, always `null` \(the recent verb does not accept a query\)\. Pinned as string-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\)\./,
    docsLabel: "memory_read.query type-level pin citation",
    docsTemplate: "Pinned as string-or-null by the schema test (`main.rs:N-M`).",
  },
  {
    field: "limit",
    selector: 'value["limit"].is_u64(),',
    docsRegex:
      /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10` for both verbs, per `main\.rs:\d+` and `main\.rs:\d+`\)\. Pinned as u64 at the schema test \(`main\.rs:(\d+)-(\d+)`\)\./,
    docsLabel: "memory_read.limit type-level pin citation",
    docsTemplate: "Pinned as u64 at the schema test (`main.rs:N-M`).",
  },
  {
    field: "min_relevance",
    selector:
      'value["min_relevance"].is_f64() || value["min_relevance"].is_null(),',
    docsRegex:
      /- `min_relevance` \(number or null\): for `mode="search"`, the float echoed from `--min-relevance` \(validated to a finite `f32` in `\[0\.0, 1\.0\]` at `main\.rs:\d+-\d+`\), or `null` when the flag was omitted\. For `mode="recent"`, always `null`\. Pinned as f64-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never a string\./,
    docsLabel: "memory_read.min_relevance type-level pin citation",
    docsTemplate:
      "Pinned as f64-or-null by the schema test (`main.rs:N-M`) — never a string.",
  },
];

const errors = [];
const fail = (message) => errors.push(message);

let docs;
let source;
try {
  docs = read(docsPath);
} catch (error) {
  fail(`cannot read ${docsPath}: ${error.message}`);
}
try {
  source = read(sourcePath);
} catch (error) {
  fail(`cannot read ${sourcePath}: ${error.message}`);
}

function scanBraceBalance(lines, openerLine) {
  let depth = 0;
  let opened = false;
  for (let index = openerLine - 1; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return index + 1;
    }
  }
  return null;
}

if (source) {
  const lines = source.split("\n");
  const testOpenerRegex = new RegExp(`^\\s+fn\\s+${testFnName}\\s*\\(`);
  const testMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (testOpenerRegex.test(lines[index])) {
      testMatches.push(index + 1);
    }
  }
  if (testMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_read pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
    } else {
      for (const target of targets) {
        const selectorMatches = [];
        for (let index = testStart; index < testEnd; index += 1) {
          if (lines[index].trim() === target.selector) {
            selectorMatches.push(index + 1);
          }
        }
        if (selectorMatches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion is present exactly once in this test`,
          );
          continue;
        }
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-to-closing range convention requires the assert!( opener on the line directly above the selector`,
          );
          continue;
        }
        target.startLine = assertOpenerLine;
        let endLine = null;
        for (let index = selectorLine; index < testEnd; index += 1) {
          if (lines[index].trim() === ");") {
            endLine = index + 1;
            break;
          }
        }
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the closing \`);\` after the ${target.field} selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
          );
          continue;
        }
        target.endLine = endLine;
      }
    }
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} type-level pin line range`,
      );
      continue;
    }
    if (target.startLine !== undefined && target.endLine !== undefined) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${citedStart}-${citedEnd} but the ${target.field} type-level assertion spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-memory-read-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-read-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
