#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume mode type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 607 cites two main.rs lines for
// the type-level pin on `value["mode"].is_string()` inside the
// two-shape intents_resume envelope. Both
// `intents_resume_ok_json_pins_top_level_schema` (success branch)
// and `intents_resume_error_json_pins_top_level_schema` (error
// branch) carry the same single-line assertion, so the docs cite is
// a pair of `main.rs:N` numbers, mirroring the dual-citation shape
// established by validate-intents-resume-kind-literal-value-pin-
// line-refs.mjs but for a type-level rather than value-level pin.
//
// Selector form: the full single-line assertion
// `assert!(value["mode"].is_string(), "mode must be a string: {value}");`
// appears in THREE test fns in main.rs — once each in the
// intents_resume_ok pins-test (line 5375), the intents_resume_error
// pins-test (line 5223), and the memory_read pins-test (line 6833).
// Brace-scoping to the intents_resume pins-test fns isolates the
// two branch matches from the memory_read sibling at line 6833.
//
// docsRegex anchoring: lines 605 (kind) and 606 (ok) both contain
// "Pinned at the value level by" wording for adjacent value-level
// pins. The regex anchors on the `- \`mode\` (string)` bullet head
// and the `"explicit"` / `"latest"` value enumeration prose unique
// to this bullet, then captures the two `main.rs:N` line refs from
// the appended type-level pin sentence. Line 607's existing prose
// about the CLI form derivation (main.rs:3889) is preserved by the
// `[^\n]*` bridge between the bullet head and the new Pinned-as
// sentence.

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
  'assert!(value["mode"].is_string(), "mode must be a string: {value}");';

const docsRegex =
  /- `mode` \(string\): exactly `"explicit"` or `"latest"`[^\n]*Pinned as a string by `main\.rs:(\d+)` \(success branch\) and `main\.rs:(\d+)` \(error branch\) — never an object or array\./;
const docsLabel = "intents_resume mode type-level pin citation";
const docsTemplate =
  '- `mode` (string): exactly `"explicit"` or `"latest"` … Pinned as a string by `main.rs:N1` (success branch) and `main.rs:N2` (error branch) — never an object or array.';

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
      error: `expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the mode type-level assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the mode type-level pin lines for both branches`,
    );
  } else if (okLine !== null && errorLine !== null) {
    const citedOk = parseInt(match[1], 10);
    const citedError = parseInt(match[2], 10);
    if (citedOk !== okLine || citedError !== errorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedOk} (success) and main.rs:${citedError} (error) but the mode type-level assertions live at :${okLine} (success) and :${errorLine} (error); remediation: update the citation to :${okLine} (success branch) and :${errorLine} (error branch)`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-mode-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-mode-type-level-pin-line-refs: ok (mode success main.rs:${okLine} / error main.rs:${errorLine})`,
);
