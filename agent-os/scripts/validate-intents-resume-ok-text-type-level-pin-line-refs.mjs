#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume_ok text type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 613 cites a main.rs line for the
// type-level pin on `value["text"].is_string()` inside
// `intents_resume_ok_json_pins_top_level_schema`. The assertion is
// single-line — `assert!(value["text"].is_string(), "text must be a
// string: {value}");` — so the docs cite is `main.rs:N` (one number),
// not the multi-line range `main.rs:N-M` used by the sibling
// validate-intents-resume-type-level-pin-line-refs.mjs targets which
// pin multi-line `assert!(...)` blocks. The shared multi-target
// validator's "assert-opener" and "selector-to-closer" conventions
// both assume a multi-line block; neither fits a single-line cite,
// so this slice ships a dedicated single-target validator modeled on
// the kind-literal-value-pin sweep (single-line cite, single
// capture group) rather than extending the multi-target validator
// with a third convention.
//
// Selector form: the full single-line assertion
// `assert!(value["text"].is_string(), "text must be a string: {value}");`
// appears in two test fns in main.rs — once in
// intents_resume_ok_json_pins_top_level_schema (line 5384, the
// target here) and once in intent_result_json_pins_top_level_schema
// (line 5793, the sibling envelope's pin). Brace-scoping to the
// intents_resume_ok pins-test isolates this match.
//
// docsRegex anchoring: line 237 carries a sibling `text` bullet for
// the intent_result envelope ("- `text` (string): the result text
// the daemon returned. The unsuffixed CLI prints this value
// directly at `main.rs:2069`"); that bullet currently lacks a
// Pinned-as sentence, but a future intent_result text pin slice
// would add the same "Pinned as a string by `main.rs:N`" wording.
// The regex anchors on the intents_resume-specific bullet head
// `- \`text\` (string) — the result text the daemon returned for
// the resumed intent.` (em-dash separator and "resumed intent"
// phrase), which neither the line 237 bullet (colon separator, no
// "resumed") nor any other docs bullet contains.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "intents_resume_ok_json_pins_top_level_schema";
const selector =
  'assert!(value["text"].is_string(), "text must be a string: {value}");';

const docsRegex =
  /- `text` \(string\) — the result text the daemon returned for the resumed intent\. The unsuffixed CLI prints this value directly at `main\.rs:\d+`\. Pinned as a string by `main\.rs:(\d+)` — never an object or array\./;
const docsLabel = "intents_resume_ok.text type-level pin citation";
const docsTemplate =
  '- `text` (string) — the result text the daemon returned for the resumed intent. The unsuffixed CLI prints this value directly at `main.rs:N`. Pinned as a string by `main.rs:N` — never an object or array.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the intents_resume_ok pins-test still exists and is not renamed or duplicated`,
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
    "validate-intents-resume-ok-text-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-ok-text-type-level-pin-line-refs: ok (text main.rs:${selectorLine})`,
);
