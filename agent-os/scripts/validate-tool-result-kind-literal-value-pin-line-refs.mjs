#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_result kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 157 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("tool_result")` inside
// `tool_result_json_pins_top_level_schema`. The envelope literal
// `tool_result` is singular (not `tools_result`) and the docs
// bullet carries an inline singular-vs-plural note between the
// literal and the Pinned-at sentence, so the docsRegex below uses
// the permissive `[^\n]*` bridge (same as tool_list,
// capability_list, peer_revoke, and receipt_list).
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("tool_result"));`
// appears exactly once in main.rs (line 7031). The
// `"tool_result"` literal in the selector and the brace-scoping
// to tool_result_json_pins_top_level_schema together isolate
// this match.
//
// Cross-line collision note: this envelope's sibling bullets at
// docs lines 159/160 cite other main.rs ranges (7033-7036 for
// `content`, 7037-7040 for `is_error`) and the schema-test
// prose lines 162/164 cite 7015/6994/7015, but those sentences
// use phrasings like `Pinned as an array`, `pinned as a JSON
// boolean by the schema test`, and `pinned to exactly these four
// by the test at` rather than `Pinned at the value level by`, so
// the docsRegex below cannot match them. Line 140 is the sibling
// `"tool_list"` envelope kind bullet with a different literal
// anchor, so cross-bullet capture is impossible.
//
// Bullet-line shape: mid-line prose between the literal and the
// Pinned-at sentence. Both ends of the `[^\n]*` bridge are
// uniquely anchored on line 157: the opening `"tool_result"` and
// the closing `Some("tool_result")` cite.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "tool_result_json_pins_top_level_schema";
const selector = 'assert_eq!(value["kind"].as_str(), Some("tool_result"));';

const docsRegex =
  /- `kind`: literal string `"tool_result"`[^\n]*Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("tool_result"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "tool_result kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"tool_result"` … Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("tool_result")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the tool_result pins-test still exists under its symmetric fn name tool_result_json_pins_top_level_schema`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the kind literal value pin assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the kind literal value pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the kind literal value pin assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-tool-result-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-result-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
