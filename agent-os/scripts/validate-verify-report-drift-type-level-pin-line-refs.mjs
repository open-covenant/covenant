#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// verify_report drift type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 118 cites a main.rs line for the
// type-level pin on `value["drift"].is_array()` inside
// `verify_report_json_pins_top_level_schema`. The assertion is
// single-line — `assert!(value["drift"].is_array(), "drift must be
// an array: {value}",);` — with a trailing comma the selector must
// reproduce exactly.
//
// Selector form: the single-line assertion is unique inside main.rs
// (one occurrence at line 7238). Brace-scoping to verify_report_
// json_pins_top_level_schema isolates the match defensively.
//
// docsRegex anchoring: line 117's adjacent `checks` bullet uses the
// same `(array of \`Verify*\`): per-check results, see below.`
// shape and already cites `main.rs:7234-7237`. Both bullets share
// the "see below." trailer and the `never null or a string` Pinned-
// as phrasing template. The regex anchors on the verify_report-
// specific `array of \`VerifyDrift\`): correlation gaps` phrase
// that line 117 lacks, then captures the appended Pinned-as line
// ref. Per the IPC docs pin collision anchor feedback memory, the
// regex extends past the shared "never null or a string" prefix
// with the unique "blob" suffix to prevent first-match capture of
// the wrong bullet.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "verify_report_json_pins_top_level_schema";
const selector =
  'assert!(value["drift"].is_array(), "drift must be an array: {value}",);';

const docsRegex =
  /- `drift` \(array of `VerifyDrift`\): correlation gaps, see below\. Pinned as an array by `main\.rs:(\d+)` — never null or a string blob\./;
const docsLabel = "verify_report.drift type-level pin citation";
const docsTemplate =
  '- `drift` (array of `VerifyDrift`): correlation gaps, see below. Pinned as an array by `main.rs:N` — never null or a string blob.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the verify_report pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the drift type-level assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the drift type-level pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the drift type-level assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-verify-report-drift-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-verify-report-drift-type-level-pin-line-refs: ok (drift main.rs:${selectorLine})`,
);
