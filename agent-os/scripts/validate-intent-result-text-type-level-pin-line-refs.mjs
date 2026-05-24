#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intent_result text type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 237 cites a main.rs line for the
// type-level pin on `value["text"].is_string()` inside
// `intent_result_json_pins_top_level_schema`. The assertion is
// single-line — `assert!(value["text"].is_string(), "text must be a
// string: {value}");` — so the docs cite is `main.rs:N` (one
// number), matching the sibling single-line type-level pin
// validate-intents-resume-ok-text-type-level-pin-line-refs.mjs
// rather than the multi-line range cites used by
// validate-intent-result-type-level-pin-line-refs.mjs.
//
// Selector form: the full single-line assertion
// `assert!(value["text"].is_string(), "text must be a string: {value}");`
// appears in two test fns in main.rs — once in
// intent_result_json_pins_top_level_schema (line 5793, the target
// here) and once in intents_resume_ok_json_pins_top_level_schema
// (line 5384, the sibling envelope's pin already wired in by the
// preceding slice). Brace-scoping to intent_result_json_pins_
// top_level_schema isolates this match.
//
// docsRegex anchoring: line 613 carries a sibling `text` bullet for
// the intents_resume_ok envelope, with em-dash separator (`—`),
// "resumed intent" phrase, and a Pinned-as cite for `main.rs:5384`.
// Line 237's intent_result bullet uses colon separator (`:`), no
// "resumed" phrase, and cites the unsuffixed CLI's main.rs:2069
// `println!("{text}")` site. The regex anchors on the colon-and-
// "the result text the daemon returned." prefix plus the unique
// `main.rs:2069` CLI cite, then captures the Pinned-as line ref.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "intent_result_json_pins_top_level_schema";
const selector =
  'assert!(value["text"].is_string(), "text must be a string: {value}");';

const docsRegex =
  /- `text` \(string\): the result text the daemon returned\. The unsuffixed CLI prints this value directly at `main\.rs:2069`[^\n]*Pinned as a string by `main\.rs:(\d+)` — never an object or array\./;
const docsLabel = "intent_result.text type-level pin citation";
const docsTemplate =
  '- `text` (string): the result text the daemon returned. The unsuffixed CLI prints this value directly at `main.rs:2069` … Pinned as a string by `main.rs:N` — never an object or array.';

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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the text type-level assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the text type-level pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the text type-level assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intent-result-text-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intent-result-text-type-level-pin-line-refs: ok (text main.rs:${selectorLine})`,
);
