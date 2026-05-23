#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peer_list envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites three inner assertion ranges
// inside peer_list_json_pins_top_level_schema:
//
//   - line 294 cites `filter_pubkey_prefix` (string or null) type pin.
//   - line 295 cites `matched_count` (u64) type pin.
//   - line 298 cites `truncated` (boolean) type pin.
//
// The existing validate-peer-list-line-refs.mjs covers the helper fn,
// renders test, and pins test declaration lines, but not the inner
// type-level selector ranges. All three cites were stale by ~7 lines
// before this validator landed.
//
// The validator scopes each lookup to the brace-balanced
// `peer_list_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test cannot contaminate
// the result. Each target's selector is matched on its first line —
// either as an exact-match (single-line selectors) or as a
// startsWith match (the multi-line filter_pubkey_prefix selector,
// which is broken across two lines as `is_string()` then `|| is_null(),`).
// Each range ends at the next closing `);` line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "peer_list_json_pins_top_level_schema";

const targets = [
  {
    field: "filter_pubkey_prefix",
    selectorFirstLine: 'value["filter_pubkey_prefix"].is_string()',
    match: "startsWith",
    docsRegex:
      /- `filter_pubkey_prefix` \(string or null\): the prefix echoed from `--prefix`, or `null` when the flag was omitted\. Pinned at the type level by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never an integer or array\./,
    docsLabel: "peer_list.filter_pubkey_prefix type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N-M`) — never an integer or array.",
  },
  {
    field: "matched_count",
    selectorFirstLine: 'value["matched_count"].is_u64(),',
    match: "exact",
    docsRegex:
      /- `matched_count` \(u64\): row count of the `peers` array; equals the exhaustive match count when `truncated` is `false`\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string\./,
    docsLabel: "peer_list.matched_count type-level pin citation",
    docsTemplate:
      "Pinned as u64 by `main.rs:N-M` — never a string.",
  },
  {
    field: "truncated",
    selectorFirstLine: 'value["truncated"].is_boolean(),',
    match: "exact",
    docsRegex:
      /Pinned as a JSON boolean by the schema test at `main\.rs:(\d+)-(\d+)` — never `0`\/`1`\./,
    docsLabel: "peer_list.truncated type-level pin citation",
    docsTemplate:
      "Pinned as a JSON boolean by the schema test at `main.rs:N-M` — never `0`/`1`.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the peer_list pins-test still exists and is not renamed or duplicated`,
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
          const trimmed = lines[index].trim();
          const matched =
            target.match === "exact"
              ? trimmed === target.selectorFirstLine
              : trimmed.startsWith(target.selectorFirstLine);
          if (matched) {
            selectorMatches.push(index + 1);
          }
        }
        if (selectorMatches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selectorFirstLine}\` (${target.match} match) inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion's first line is present exactly once in this test`,
          );
          continue;
        }
        const startLine = selectorMatches[0];
        let endLine = null;
        for (let index = startLine; index < testEnd; index += 1) {
          if (lines[index].trim() === ");") {
            endLine = index + 1;
            break;
          }
        }
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the closing \`);\` after the ${target.field} selector at line ${startLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
          );
          continue;
        }
        target.startLine = startLine;
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
  console.error("validate-peer-list-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-list-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
