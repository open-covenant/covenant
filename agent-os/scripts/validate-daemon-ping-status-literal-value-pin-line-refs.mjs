#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// daemon_ping status literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 226 cites a main.rs line for the
// value-level pin on `value["status"].as_str() == Some("ok")` inside
// `ping_json_pins_top_level_schema`. The sibling is_string type pin
// at main.rs:5854-5857 is already guarded by
// validate-ping-type-level-pin-line-refs.mjs; this validator binds
// the docs prose to the status-literal value assertion so a future
// rename of the success literal (for example, splitting `"ok"` into
// `"ready"` vs `"alive"`) would fail at the docs-validator level
// rather than only at test runtime.
//
// Selector form: the single-line statement
// `assert_eq!(value["status"].as_str(), Some("ok"));` appears
// exactly once in main.rs (line 5858). Many sibling envelopes assert
// their own kind literal with the same macro shape but a different
// field key (`value["kind"]`) or a different `Some(...)` argument;
// the `value["status"]` selector plus the brace-scoping to
// ping_json_pins_top_level_schema isolate this match from every
// kind-literal pin and from any future sibling status-literal pin.
//
// Same single-line single-cite pattern as the sibling
// validate-daemon-ping-kind-literal-value-pin-line-refs.mjs: the
// docs cite is `main.rs:N` (one number), not `main.rs:N-M` (a
// range).
//
// docsRegex anchoring: line 226's bullet already contains the
// adjacent type-level Pinned-as sentence. The value-level sentence
// is appended after the "— never an integer or boolean." period.
// The regex anchors on the unique value-level phrase
// `asserts `value["status"].as_str() == Some("ok")`` plus the
// "future status-rename fails the test" trailer that no sibling
// bullet contains, preventing first-match collision with the
// daemon_ping kind-literal value pin sentence at line 225.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "ping_json_pins_top_level_schema";
const selector = 'assert_eq!(value["status"].as_str(), Some("ok"));';

const docsRegex =
  /The literal value `"ok"` is also pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["status"\]\.as_str\(\) == Some\("ok"\)`\), so a future status-rename fails the test rather than silently rewriting the literal\./;
const docsLabel = "daemon_ping status literal value pin citation";
const docsTemplate =
  'The literal value `"ok"` is also pinned at the value level by `main.rs:N` (asserts `value["status"].as_str() == Some("ok")`), so a future status-rename fails the test rather than silently rewriting the literal.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the daemon_ping pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the status literal value pin assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the status literal value pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the status literal value pin assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-daemon-ping-status-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-daemon-ping-status-literal-value-pin-line-refs: ok (status-value main.rs:${selectorLine})`,
);
