#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// receipt_list envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites three inner assertion ranges
// inside the receipt_list pins test:
//
//   - line 198 cites `main.rs:5490-5493` for the `limit` (u64) type
//     pin.
//   - line 199 cites `main.rs:5494-5497` for the `since_ms`
//     (u64 or null) type pin.
//   - line 200 cites `main.rs:5498-5501` for the `receipts` (array)
//     type pin.
//
// The existing validate-receipt-list-line-refs.mjs covers the helper fn,
// renders test, and pins test declaration lines, but not the inner
// assertion ranges. A future edit that adds, removes, or reorders an
// inner assert inside the receipt_list pins test would silently shift
// these line ranges while the docs cites stayed unchanged.
//
// This validator scopes its lookups to the brace-balanced
// `receipt_list_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test cannot contaminate
// the result.
//
// The docs convention cites the range from the `assert!(` opener line
// directly above the selector match through the closing `);` of the
// surrounding `assert!(...)` macro call (assert!-opener-to-closer
// 4-line convention, mirroring validate-peer-list-type-level-pin-line
// -refs.mjs and validate-capability-list-type-level-pin-line-refs.mjs).
// The opener-to-closer convention catches a wider set of drift modes
// than the prior body-to-closer convention: a body-to-closer cite
// stays correct silently when the `assert!(` opener moves up/down by
// 1 line while the opener-to-closer cite fails loudly.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "receipt_list_json_pins_top_level_schema";

const targets = [
  {
    field: "limit",
    selector: 'value["limit"].is_u64(),',
    docsRegex:
      /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, per `main\.rs:\d+`\)\. Pinned at the type level by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never a string\./,
    docsLabel: "receipt_list.limit type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N-M`) — never a string.",
  },
  {
    field: "since_ms",
    selector: 'value["since_ms"].is_u64() || value["since_ms"].is_null(),',
    docsRegex:
      /- `since_ms` \(u64 or null\): the Unix-epoch millisecond threshold echoed from `--since-ms`, or `null` when the flag was omitted\. Pinned as u64-or-null at the schema test \(`main\.rs:(\d+)-(\d+)`\) — never a string-of-integer\./,
    docsLabel: "receipt_list.since_ms type-level pin citation",
    docsTemplate:
      "Pinned as u64-or-null at the schema test (`main.rs:N-M`) — never a string-of-integer.",
  },
  {
    field: "receipts",
    selector: 'value["receipts"].is_array(),',
    docsRegex:
      /- `receipts` \(array of `SettlementReceipt`\): the matched receipts in the order returned by the daemon\. The array is empty when no receipts fall in the window; the unsuffixed CLI prints `\(no receipts\)` for that case at `main\.rs:\d+`\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "receipt_list.receipts type-level pin citation",
    docsTemplate:
      "Pinned as an array by `main.rs:N-M` — never null or a string.",
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

let testStart = null;
let testEnd = null;
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the receipt_list pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    testStart = testMatches[0];
    testEnd = scanBraceBalance(lines, testStart);
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
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector for the ${target.field} type pin`,
          );
          continue;
        }
        const startLine = assertOpenerLine;
        let endLine = null;
        for (let index = selectorLine; index < testEnd; index += 1) {
          if (lines[index].trim() === ");") {
            endLine = index + 1;
            break;
          }
        }
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the closing \`);\` after \`${target.selector}\` at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
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
  console.error("validate-receipt-list-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-receipt-list-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
