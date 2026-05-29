#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_compaction outcome.parents_detached empty-case pin line-ref
// drift guard. docs/ipc-and-http-gateway.md line 552 cites a main.rs
// range for the renders-test sibling pin on
// `value["outcome"]["parents_detached"].as_array().map(Vec::len) == Some(0)`
// inside `memory_compaction_json_renders_stable_shape` (the empty
// dry_run-case fixture).
//
// Selector form: the assert_eq! body is split across multiple lines,
// so the trimmed first line is `value["outcome"]["parents_detached"]`
// (no suffix). Matching that exact trim isolates the renders-test
// occurrence from any future schema-test pin that might use the
// `.is_array(),` suffix form. Brace-scoping to
// memory_compaction_json_renders_stable_shape adds a second layer of
// isolation against future test-fn additions.
//
// The range is derived as assert_eq!-opener-to-closer (the cite spans
// the `assert_eq!(` opener directly above the selector through the
// closing `);` on its own line), matching the sibling
// validate-memory-compaction-plan-records-empty-renders-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "memory_compaction_json_renders_stable_shape";
const selector = 'value["outcome"]["parents_detached"]';

const docsRegex =
  /- `parents_detached` \(array of strings\) — UUIDs of records whose parent pointer the policy detached \(or would detach, when `--detach-stale-parents` is supplied\)\. The empty-case is pinned by the stable-shape test at `main\.rs:(\d+)-(\d+)` \(asserts `value\["outcome"\]\["parents_detached"\]\.as_array\(\)\.map\(Vec::len\) == Some\(0\)`\)\./;
const docsLabel =
  "memory_compaction outcome.parents_detached empty-case renders pin citation";
const docsTemplate =
  "The empty-case is pinned by the stable-shape test at `main.rs:N-M` (asserts `value[\"outcome\"][\"parents_detached\"].as_array().map(Vec::len) == Some(0)`).";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_compaction renders_stable_shape test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the outcome.parents_detached empty-case pin assertion is present exactly once in this test`,
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
              `${sourcePath}: could not find the closing \`);\` after the outcome.parents_detached selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the outcome.parents_detached empty-case renders pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the outcome.parents_detached empty-case pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-compaction-outcome-parents-detached-empty-renders-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compaction-outcome-parents-detached-empty-renders-pin-line-refs: ok (outcome.parents_detached main.rs:${startLine}-${endLine})`,
);
