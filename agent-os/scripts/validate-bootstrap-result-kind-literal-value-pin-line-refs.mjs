#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// bootstrap_result kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 591 cites a main.rs line for the
// value-level pin on
// `value["kind"].as_str() == Some("bootstrap_result")` inside
// `bootstrap_result_json_pins_top_level_schema`. The sibling
// is_string type pin at main.rs:5699 is the type-level guard; this
// validator binds the docs prose to the discriminator-value
// assertion so a future kind-rename (for example, splitting the
// envelope into a tagged-enum sibling) would fail at the
// docs-validator level rather than only at test runtime.
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("bootstrap_result"));`
// appears exactly once in main.rs (line 5700). Many sibling
// envelopes assert their own kind literal with the same macro
// signature but a different `Some(...)` argument; the
// `"bootstrap_result"` literal in the selector and the
// brace-scoping to bootstrap_result_json_pins_top_level_schema
// together isolate this match from peer_list, peer_revoke,
// intents_resume, intent_result, daemon_ping, capability_list,
// and every other kind-literal pin in this file.
//
// Unlike the multi-line assert_eq! patterns used by
// validate-{memory,settlement}-backfill-schema-value-pin-line-refs.mjs,
// the kind literal pin here is a single-line statement, so the
// docs cite is `main.rs:N` (one number), not `main.rs:N-M` (a
// range). The validator pins the single line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "bootstrap_result_json_pins_top_level_schema";
const selector =
  'assert_eq!(value["kind"].as_str(), Some("bootstrap_result"));';

const docsRegex =
  /- `kind`: literal string `"bootstrap_result"`\. Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("bootstrap_result"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "bootstrap_result kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"bootstrap_result"`. Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("bootstrap_result")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the bootstrap_result pins-test still exists and is not renamed or duplicated`,
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
    "validate-bootstrap-result-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-bootstrap-result-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
