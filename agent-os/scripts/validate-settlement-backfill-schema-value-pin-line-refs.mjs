#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement_backfill schema literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 634 cites a main.rs range for the
// value-level pin on
// `value["schema"].as_str() == Some("covenant.settlement.backfill.v1")`
// inside `settlement_backfill_json_pins_top_level_schema`. The sibling
// is_string type pin at main.rs:5565-5568 is already cited; this
// validator binds the docs prose to the value-level assertion so a
// future `.v2` bump that silently rewrote the schema literal would
// fail at the docs-validator level (not just at test runtime).
//
// Selector form: the trimmed first line `value["schema"].as_str(),`
// appears 2x in main.rs:
//
//   - settlement_backfill_json_pins_top_level_schema body at 5570
//     (this validator's target).
//   - memory_backfill_json_pins_top_level_schema body at 5633
//     (sibling envelope's analogous value pin).
//
// Brace-scoping to settlement_backfill_json_pins_top_level_schema
// isolates 5570 from 5633. Both selectors are first lines of
// multi-line assert_eq! bodies in their respective pin tests.
//
// The range is derived as assert_eq!-opener-to-closer (the cite spans
// the `assert_eq!(` opener directly above the selector through the
// closing `);` on its own line), matching the sibling
// validate-intents-resume-error-ok-false-pin-line-refs.mjs (another
// value-pin validator).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "settlement_backfill_json_pins_top_level_schema";
const selector = 'value["schema"].as_str(),';

const docsRegex =
  /The literal value `"covenant\.settlement\.backfill\.v1"` is also pinned at the value level by `main\.rs:(\d+)-(\d+)` \(asserts `value\["schema"\]\.as_str\(\) == Some\("covenant\.settlement\.backfill\.v1"\)`\), so a future `\.v2` bump fails the test rather than silently rewriting the schema string\./;
const docsLabel = "settlement_backfill schema value pin citation";
const docsTemplate =
  "The literal value `\"covenant.settlement.backfill.v1\"` is also pinned at the value level by `main.rs:N-M` (asserts `value[\"schema\"].as_str() == Some(\"covenant.settlement.backfill.v1\")`), so a future `.v2` bump fails the test rather than silently rewriting the schema string.";

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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the schema value pin assertion is present exactly once in this test`,
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
              `${sourcePath}: could not find the closing \`);\` after the schema selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the schema value pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the schema value pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-settlement-backfill-schema-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-schema-value-pin-line-refs: ok (schema-value main.rs:${startLine}-${endLine})`,
);
