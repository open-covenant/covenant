#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_compaction_plan expected_receipt_changes.records empty-case pin
// line-ref drift guard. docs/ipc-and-http-gateway.md line 569 cites a
// main.rs range for the renders-test sibling pin on
// `expected_receipt_changes.records.as_array().map(Vec::len) == Some(0)`
// inside `memory_compaction_plan_json_renders_stable_shape`.
//
// The pins_expected_receipt_changes_schema test holds the explicit
// is_array + length pins (already cited inline on the same docs
// bullet); this validator binds the docs prose to the *renders-test*
// sibling assertion so a refactor that drops the empty-case fixture
// from the stable-shape test surfaces at the docs-validator level
// rather than only at test runtime.
//
// Selector collision risk: the trimmed first line of the assert_eq!
// body, `value["expected_receipt_changes"]["records"]`, appears twice
// in main.rs:
//
//   - memory_compaction_plan_json_renders_stable_shape body at 6645
//     (this validator's target).
//   - memory_compaction_plan_json_pins_expected_receipt_changes_schema
//     body at 6731 (the sibling length pin, identical trim).
//
// Both risks are addressed by scoping the selector lookup to the
// brace-balanced `memory_compaction_plan_json_renders_stable_shape`
// fn body. A third occurrence at 6727 carries the `.is_array(),`
// suffix and is excluded by the exact-trim match.
//
// The range is derived as assert_eq!-opener-to-closer (the cite spans
// the `assert_eq!(` opener directly above the selector through the
// closing `);` on its own line), matching the sibling
// validate-audit-verify-report-failures-empty-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "memory_compaction_plan_json_renders_stable_shape";
const selector = 'value["expected_receipt_changes"]["records"]';

const docsRegex =
  /The renders-test sibling at `main\.rs:(\d+)-(\d+)` independently pins the same empty-case `expected_receipt_changes\.records` assertion \(`as_array\(\)\.map\(Vec::len\) == Some\(0\)`\)\./;
const docsLabel =
  "memory_compaction_plan expected_receipt_changes.records renders-test sibling pin citation";
const docsTemplate =
  "The renders-test sibling at `main.rs:N-M` independently pins the same empty-case `expected_receipt_changes.records` assertion (`as_array().map(Vec::len) == Some(0)`).";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_compaction_plan renders_stable_shape test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the expected_receipt_changes.records empty-case pin assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert_eq!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert_eq!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert_eq!-opener-to-closer convention requires the assert_eq!( opener on the line directly above the selector`,
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
              `${sourcePath}: could not find the closing \`);\` after the expected_receipt_changes.records selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the renders-test sibling pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the expected_receipt_changes.records empty-case pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-compaction-plan-records-empty-renders-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compaction-plan-records-empty-renders-pin-line-refs: ok (expected_receipt_changes.records main.rs:${startLine}-${endLine})`,
);
