#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement_backfill envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 635 cites a single main.rs range
// for the settlement_backfill.row_count (u64) type pin inside the
// settlement_backfill_json_pins_top_level_schema fn body.
//
// Before this validator landed, the docs prose described `row_count`
// as "(u64): count of legacy settlement-receipt rows the backfill
// operated on (mutation path) or *would* operate on (dry-run path)"
// with no main.rs cite. The assertion at main.rs:5574-5577
// (assert!(value["row_count"].is_u64(), ...);) was only enforced at
// test runtime. The validator binds the docs prose to the source-of-
// truth so a regression that turned the field into a string-of-integer
// surfaces at the docs-validator level (not just at test runtime).
//
// Sibling collision risk: the memory_backfill envelope's pins test at
// main.rs:5613 carries the same `value["row_count"].is_u64(),` selector
// at main.rs:5638, and the memory_backfill row_count docs bullet at
// line 650 carries near-identical prose. Both risks are addressed:
//
//   - The validator scopes the selector lookup to the brace-balanced
//     `settlement_backfill_json_pins_top_level_schema` fn body, so the
//     memory_backfill occurrence at main.rs:5638 cannot contaminate
//     the result.
//   - The docsRegex anchors on the settlement-specific phrase
//     "legacy settlement-receipt rows the backfill operated on" and
//     the settlement-specific closer "the verb does not error on an
//     empty backfill". The memory_backfill bullet at line 650 uses
//     "memory records the correlation pass operated on" instead and
//     omits the empty-backfill closer, so the regex will not match it
//     even on a first-match scan.
//
// The range is derived as assert!-opener-to-closer (4-line convention)
// — the cite spans the `assert!(` opener directly above the selector
// through the closing `);` on its own line, mirroring
// validate-a2a-compact-type-level-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "settlement_backfill_json_pins_top_level_schema";
const selector = 'value["row_count"].is_u64(),';

const docsRegex =
  /- `row_count` \(u64\): count of legacy settlement-receipt rows the backfill operated on \(mutation path\) or \*would\* operate on \(dry-run path\)\. May legitimately be `0` when no legacy rows match — the verb does not error on an empty backfill\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string-of-integer\./;
const docsLabel = "settlement_backfill.row_count type-level pin citation";
const docsTemplate =
  "Pinned as u64 by `main.rs:N-M` — never a string-of-integer.";

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

let startLine = null;
let endLine = null;
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the settlement_backfill pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
    } else {
      const selectorMatches = [];
      for (let index = testStart; index < testEnd; index += 1) {
        if (lines[index].trim() === selector) {
          selectorMatches.push(index + 1);
        }
      }
      if (selectorMatches.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the row_count type-level assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector`,
          );
        } else {
          startLine = assertOpenerLine;
          for (let index = selectorLine; index < testEnd; index += 1) {
            if (lines[index].trim() === ");") {
              endLine = index + 1;
              break;
            }
          }
          if (endLine === null) {
            fail(
              `${sourcePath}: could not find the closing \`);\` after the row_count selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
            );
          }
        }
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the row_count type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the row_count type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-settlement-backfill-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-type-level-pin-line-refs: ok (settlement_backfill.row_count main.rs:${startLine}-${endLine})`,
);
