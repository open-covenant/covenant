#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 605 cites two main.rs lines for
// the value-level pin on
// `value["kind"].as_str() == Some("intents_resume")` inside the
// two-shape (ok/error) intents_resume envelope. Both
// `intents_resume_ok_json_pins_top_level_schema` (success branch)
// and `intents_resume_error_json_pins_top_level_schema` (error
// branch) carry the same single-line assertion, so the docs cite
// is a pair of `main.rs:N` numbers — not a range like the
// ok=false value-pin sibling validator's `main.rs:N-M`.
//
// This is the only kind-literal-value pin in this docs file with
// two emitter helpers and two pins-test fns; sibling pin
// validators (bootstrap_result, intent_result, daemon_ping, etc.)
// each bind a single test fn. The validator finds both test fns,
// confirms each contains the kind-value assertion exactly once,
// and verifies the docs cite is ordered `(success branch)` first,
// `(error branch)` second.
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("intents_resume"));`
// appears exactly twice in main.rs — once per branch's pins-test
// fn body. Brace-scoping to each test fn isolates the per-branch
// match; the `"intents_resume"` literal in the selector
// disambiguates from every other kind-literal pin in the file.
//
// docsRegex anchoring: the bullet's mid-line prose contains a
// disambiguator clause about verb-name asymmetry plus the
// "emitted on both `ok=true` and `ok=false`" anchor, which is
// unique to this bullet. Line 606's adjacent ok=false value-pin
// sentence also contains "Pinned at the value level by" wording,
// so the regex must anchor on the `"intents_resume"` literal
// plus the `(success branch)` and `(error branch)` disambiguator
// wording — neither phrase appears on line 606.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const okTestFnName = "intents_resume_ok_json_pins_top_level_schema";
const errorTestFnName = "intents_resume_error_json_pins_top_level_schema";
const selector =
  'assert_eq!(value["kind"].as_str(), Some("intents_resume"));';

const docsRegex =
  /- `kind`: literal string `"intents_resume"`[^\n]*Pinned at the value level by `main\.rs:(\d+)` \(success branch\) and `main\.rs:(\d+)` \(error branch\) — each asserts `value\["kind"\]\.as_str\(\) == Some\("intents_resume"\)` — so a future kind-rename fails the tests rather than silently rewriting the discriminator string\./;
const docsLabel = "intents_resume kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"intents_resume"` … Pinned at the value level by `main.rs:N1` (success branch) and `main.rs:N2` (error branch) — each asserts `value["kind"].as_str() == Some("intents_resume")` — so a future kind-rename fails the tests rather than silently rewriting the discriminator string.';

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

function findSelectorInTestFn(lines, testFnName) {
  const testOpenerRegex = new RegExp(`^\\s+fn\\s+${testFnName}\\s*\\(`);
  const testMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (testOpenerRegex.test(lines[index])) {
      testMatches.push(index + 1);
    }
  }
  if (testMatches.length !== 1) {
    return {
      error: `expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the intents_resume pins-test still exists and is not renamed or duplicated`,
    };
  }
  const testStart = testMatches[0];
  const testEnd = scanBraceBalance(lines, testStart);
  if (testEnd === null) {
    return {
      error: `could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
    };
  }
  const selectorMatches = [];
  for (let index = testStart; index < testEnd; index += 1) {
    if (lines[index].trim() === selector) {
      selectorMatches.push(index + 1);
    }
  }
  if (selectorMatches.length !== 1) {
    return {
      error: `expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the kind literal value pin assertion is present exactly once in this test`,
    };
  }
  return { line: selectorMatches[0] };
}

let okLine = null;
let errorLine = null;
if (source) {
  const lines = source.split("\n");
  const okResult = findSelectorInTestFn(lines, okTestFnName);
  if (okResult.error) {
    fail(`${sourcePath}: ${okResult.error}`);
  } else {
    okLine = okResult.line;
  }
  const errorResult = findSelectorInTestFn(lines, errorTestFnName);
  if (errorResult.error) {
    fail(`${sourcePath}: ${errorResult.error}`);
  } else {
    errorLine = errorResult.line;
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the kind literal value pin lines for both branches`,
    );
  } else if (okLine !== null && errorLine !== null) {
    const citedOk = parseInt(match[1], 10);
    const citedError = parseInt(match[2], 10);
    if (citedOk !== okLine || citedError !== errorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedOk} (success) and main.rs:${citedError} (error) but the kind literal value pin assertions live at :${okLine} (success) and :${errorLine} (error); remediation: update the citation to :${okLine} (success branch) and :${errorLine} (error branch)`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-kind-literal-value-pin-line-refs: ok (kind-value success main.rs:${okLine} / error main.rs:${errorLine})`,
);
