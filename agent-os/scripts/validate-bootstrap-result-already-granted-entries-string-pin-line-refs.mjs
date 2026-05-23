#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// bootstrap_result already_granted-entries is_string asymmetric shape
// pin line-ref drift guard. docs/ipc-and-http-gateway.md line 593
// cites a main.rs range for the asymmetric inner-shape pin on
// `populated["already_granted"][0].is_string()` inside
// `bootstrap_result_json_pins_top_level_schema`. The pin enforces
// that already_granted entries are bare strings (not the
// `{action, signature_b58}` object shape of the sibling granted
// array) — the documented asymmetry between the two top-level arrays.
//
// Selector form: the trimmed line
// `populated["already_granted"][0].is_string(),` appears exactly once
// in main.rs (line 5723). The no_new_grants variant at line 5735 uses
// `no_new_grants["already_granted"][0].is_string(),` — different
// fixture variable — so the exact-trim match isolates this assertion
// from the second already_granted is_string assertion in the same
// test fn. Brace-scoping to bootstrap_result_json_pins_top_level_schema
// adds isolation against future test fn additions.
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
const selector = 'populated["already_granted"][0].is_string(),';

const docsRegex =
  /The asymmetric inner shape — `already_granted` entries are bare strings, not objects — is pinned by `main\.rs:(\d+)-(\d+)` \(asserts `populated\["already_granted"\]\[0\]\.is_string\(\)`\)\./;
const docsLabel =
  "bootstrap_result already_granted-entries is_string asymmetric shape pin citation";
const docsTemplate =
  "The asymmetric inner shape — `already_granted` entries are bare strings, not objects — is pinned by `main.rs:N-M` (asserts `populated[\"already_granted\"][0].is_string()`).";

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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the already_granted-entries is_string assertion is present exactly once in this test`,
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
              `${sourcePath}: could not find the closing \`);\` after the already_granted-entries selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the already_granted-entries is_string pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the already_granted-entries is_string pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-bootstrap-result-already-granted-entries-string-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-bootstrap-result-already-granted-entries-string-pin-line-refs: ok (already_granted-entries.is_string main.rs:${startLine}-${endLine})`,
);
