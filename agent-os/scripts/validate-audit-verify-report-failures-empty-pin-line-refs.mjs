#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// audit_verify report.failures empty-case pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 392 cites a main.rs range for the
// audit_verify report.failures empty-case Vec::len pin (the
// `valid == true` branch of `audit_verify_json_renders_stable_shape`).
//
// The cite uses the assert_eq!-opener-to-closer range convention
// established by the sibling root_hash_hex length validator and the
// memory_compaction_plan validator. The 4-line range catches drift
// even when the selector line itself stays put (e.g., a refactor
// changes the expected value from `Some(0)` to a different shape, or
// drops the trailing closer onto the selector line).
//
// Unlike the type-pin validators in this family, this validator
// scopes to `audit_verify_json_renders_stable_shape` (a stable-shape
// test), not `_pins_top_level_schema`. The empty-case pin lives in the
// former alongside the root_hash_hex length pin. The selector
// `value["report"]["failures"].as_array().map(Vec::len),` is the
// assert_eq! body's first line and is unique to this assert_eq!
// inside the renders-test fn body (no other failures pin exists yet).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "audit_verify_json_renders_stable_shape";
const selector = 'value["report"]["failures"].as_array().map(Vec::len),';

const docsRegex =
  /- `failures` \(array of strings\) — human-readable failure descriptions \(e\.g\., `"chain hash mismatch at event 3"`\), empty when `valid` is `true`\. The empty case is pinned by the stable-shape test at `main\.rs:(\d+)-(\d+)` \(asserts `as_array\(\)\.map\(Vec::len\) == Some\(0\)`\)\./;
const docsLabel = "audit_verify report.failures empty-case pin citation";
const docsTemplate =
  "The empty case is pinned by the stable-shape test at `main.rs:N-M` (asserts `as_array().map(Vec::len) == Some(0)`).";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the audit_verify renders_stable_shape test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the failures empty-case pin assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert_eq!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert_eq!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert_eq!-to-closing convention requires the assert_eq!( opener on the line directly above the selector`,
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
              `${sourcePath}: could not find the closing \`);\` after the failures selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the failures empty-case pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the failures empty-case pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-audit-verify-report-failures-empty-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-audit-verify-report-failures-empty-pin-line-refs: ok (failures main.rs:${startLine}-${endLine})`,
);
