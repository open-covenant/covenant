#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intent_result envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 239 cites a single main.rs range
// inside intent_result_json_pins_top_level_schema for the settlement
// (object or null) type pin.
//
// The existing validate-intent-result-line-refs.mjs covers the helper
// fn, renders test, and pins test declaration lines, but not the
// inner type-level selector range. The cite was stale by ~223 lines
// before this validator landed (it pointed into an unrelated
// settlement_backfill test body).
//
// The validator scopes its lookup to the brace-balanced
// `intent_result_json_pins_top_level_schema` fn body so the same
// settlement selector inside intents_resume_ok_json_pins_top_level_schema
// (and its sibling resume tests) cannot contaminate the result. The
// cite uses the selector-to-closing range convention: start at the
// line containing the selector, end at the next `);` closing line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "intent_result_json_pins_top_level_schema";
const selector =
  'value["settlement"].is_object() || value["settlement"].is_null(),';

const docsRegex =
  /- `settlement` \(object or null\): an optional `SettlementReceipt` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:\d+`\) carrying the on-chain or local settlement evidence when the intent consumed credits\. `null` when the intent did not settle \(e\.g\., a phase-0 echo that does not charge\)\. Pinned as object-or-null by `main\.rs:(\d+)-(\d+)` — never an integer or array\./;
const docsLabel = "intent_result.settlement type-level pin citation";
const docsTemplate =
  "Pinned as object-or-null by `main.rs:N-M` — never an integer or array.";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the intent_result pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the settlement type-level assertion is present exactly once in this test`,
        );
      } else {
        startLine = selectorMatches[0];
        for (let index = startLine; index < testEnd; index += 1) {
          if (lines[index].trim() === ");") {
            endLine = index + 1;
            break;
          }
        }
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the closing \`);\` after the settlement selector at line ${startLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
          );
        }
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the intent_result.settlement type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the settlement type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-intent-result-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intent-result-type-level-pin-line-refs: ok (intent_result.settlement main.rs:${startLine}-${endLine})`,
);
