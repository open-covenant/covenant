#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_result name type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 158 cites a main.rs line for the
// type-level pin on `value["name"].is_string()` inside
// `tool_result_json_pins_top_level_schema`. The assertion is
// single-line — `assert!(value["name"].is_string(), "name must be a
// string: {value}");` — so the docs cite is `main.rs:N` (one
// number), matching the kind-literal-value-pin sweep precedent.
//
// Selector form: the single-line assertion is unique inside main.rs
// (one occurrence at line 7032). Brace-scoping to tool_result_json_
// pins_top_level_schema isolates the match defensively.
//
// docsRegex anchoring: line 145 carries a sibling `name` bullet for
// the ToolSpec sub-shape (`- \`name\` (string) — tool identifier.`)
// with em-dash separator and "tool identifier" wording. The
// tool_result envelope's bullet at line 158 uses colon separator
// and "the tool name echoed back from the CLI argument" wording.
// The regex anchors on the colon-and-"echoed back" phrasing unique
// to this bullet, then captures the appended Pinned-as line ref.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "tool_result_json_pins_top_level_schema";
const selector =
  'assert!(value["name"].is_string(), "name must be a string: {value}");';

const docsRegex =
  /- `name` \(string\): the tool name echoed back from the CLI argument\. Pinned as a string by `main\.rs:(\d+)` — never an object or array\./;
const docsLabel = "tool_result.name type-level pin citation";
const docsTemplate =
  '- `name` (string): the tool name echoed back from the CLI argument. Pinned as a string by `main.rs:N` — never an object or array.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the tool_result pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the name type-level assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the name type-level pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the name type-level assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-tool-result-name-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-result-name-type-level-pin-line-refs: ok (name main.rs:${selectorLine})`,
);
