#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_status tasks type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 442 cites a main.rs line for the
// type-level pin on `value["tasks"].is_array()` inside
// `a2a_status_json_pins_top_level_schema`. The assertion is single-
// line — `assert!(value["tasks"].is_array(), "tasks must be an
// array: {value}",);` — so the docs cite is `main.rs:N` (one
// number). Note the trailing comma before the closing paren: the
// selector must reproduce it exactly so the line-trim match
// finds the assertion at line 7420.
//
// Selector form: the single-line assertion is unique inside the
// pins-test (and in main.rs as a whole — no sibling envelope
// asserts a `tasks` field). Brace-scoping to a2a_status_json_pins_
// top_level_schema isolates the match.
//
// docsRegex anchoring: line 443 carries the sibling `results`
// bullet, already pinned by `main.rs:7421-7424`. Both bullets share
// the "array may be empty" wording but differ in the leading bullet
// head — `tasks` vs `results` — and the A2ATaskQueueEntry vs
// A2ATaskResult type names. The regex anchors on the `- \`tasks\`
// (array of \`A2ATaskQueueEntry\`)` bullet head unique to this
// bullet, then captures the Pinned-as line ref appended after the
// existing "The array may be empty." trailer.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "a2a_status_json_pins_top_level_schema";
const selector =
  'assert!(value["tasks"].is_array(), "tasks must be an array: {value}",);';

const docsRegex =
  /- `tasks` \(array of `A2ATaskQueueEntry`\): the matched queue entries in the order returned by the daemon\. The array may be empty\. Pinned as an array by `main\.rs:(\d+)` — never null or a string blob\./;
const docsLabel = "a2a_status.tasks type-level pin citation";
const docsTemplate =
  '- `tasks` (array of `A2ATaskQueueEntry`): the matched queue entries in the order returned by the daemon. The array may be empty. Pinned as an array by `main.rs:N` — never null or a string blob.';

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

let selectorLine = null;
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the a2a_status pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the tasks type-level assertion is present exactly once in this test`,
        );
      } else {
        selectorLine = selectorMatches[0];
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the tasks type-level pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the tasks type-level assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-a2a-status-tasks-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-tasks-type-level-pin-line-refs: ok (tasks main.rs:${selectorLine})`,
);
