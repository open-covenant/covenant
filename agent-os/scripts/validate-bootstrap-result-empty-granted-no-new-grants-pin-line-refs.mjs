#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// bootstrap_result empty-granted (no_new_grants fixture) is_empty pin
// line-ref drift guard. docs/ipc-and-http-gateway.md line 597 cites a
// main.rs range for the empty-granted invariant pin on
// `no_new_grants["granted"].as_array().unwrap().is_empty()` inside
// `bootstrap_result_json_pins_top_level_schema`. The pin enforces
// that the empty-granted case serializes as a JSON array (`[]`), not
// `null` or an absent key — the operator-facing "nothing to do"
// signal documented at line 597.
//
// Selector form: the trimmed line
// `no_new_grants["granted"].as_array().unwrap().is_empty(),` (with
// the trailing comma) appears exactly once in main.rs (line 5731 —
// the multi-line assert! body's first line inside the pins test).
// A sibling single-line variant exists at line 5678 in
// `bootstrap_result_json_renders_stable_shape` but ends with `);` and
// is excluded by both brace-scoping and the exact-trim match.
//
// The range is derived as assert!-opener-to-closer (the cite spans
// the `assert!(` opener directly above the selector through the
// closing `);` on its own line), matching the sibling
// validate-bootstrap-result-granted-entries-object-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "bootstrap_result_json_pins_top_level_schema";
const selector = 'no_new_grants["granted"].as_array().unwrap().is_empty(),';

const docsRegex =
  /The empty-granted case must serialize as a JSON array \(`\[\]`\), not as `null` or an absent key; this invariant is pinned by `main\.rs:(\d+)-(\d+)` \(asserts `no_new_grants\["granted"\]\.as_array\(\)\.unwrap\(\)\.is_empty\(\)`\)\./;
const docsLabel =
  "bootstrap_result empty-granted no_new_grants is_empty pin citation";
const docsTemplate =
  "The empty-granted case must serialize as a JSON array (`[]`), not as `null` or an absent key; this invariant is pinned by `main.rs:N-M` (asserts `no_new_grants[\"granted\"].as_array().unwrap().is_empty()`).";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the bootstrap_result pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the empty-granted is_empty assertion is present exactly once in this test`,
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
              `${sourcePath}: could not find the closing \`);\` after the empty-granted selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the empty-granted no_new_grants is_empty pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the empty-granted no_new_grants is_empty pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-bootstrap-result-empty-granted-no-new-grants-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-bootstrap-result-empty-granted-no-new-grants-pin-line-refs: ok (empty-granted main.rs:${startLine}-${endLine})`,
);
