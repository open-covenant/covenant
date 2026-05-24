#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_status kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 436 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("a2a_status")` inside
// `a2a_status_json_pins_top_level_schema`. No name asymmetry
// here: the envelope literal, helper fn name, and test fn name
// all share the `a2a_status` token (same as receipt_batch_list,
// chain_status, verify_report, audit_recent, memory_read, and
// ignore_report).
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("a2a_status"));`
// appears exactly once in main.rs (line 7403). The
// `"a2a_status"` literal in the selector and the brace-scoping
// to a2a_status_json_pins_top_level_schema together isolate this
// match.
//
// Cross-line collision note: this envelope's sibling bullets at
// docs lines 437-442 cite other main.rs ranges (7404-7407,
// 7408-7411, 7412-7415, 7416-7419, 7421-7424) for the
// `limit`/`min_lease_age_ms`/`deadline_within_ms`/`state_filter`/
// `results` shape pins, but those sentences use phrasings like
// `Pinned as u64`, `Pinned as u64-or-null by the schema test`,
// `Pinned as string-or-null by the schema test`, and `Pinned as
// an array` rather than `Pinned at the value level by`, so the
// docsRegex below cannot match them. The `"a2a_status"` literal
// appears only on line 436 in the docs.
//
// Bullet-line shape: a2a_status's bullet has no extra inline
// prose between the literal and the Pinned-at sentence (clean
// bullet, period separator like receipt_batch_list,
// receipt_batch_flushed, chain_status, verify_report,
// audit_recent, memory_read, and ignore_report), so the
// docsRegex uses the strict `. Pinned` join rather than the
// permissive `[^\n]*` bridge used by sibling validators with
// mid-line prose.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "a2a_status_json_pins_top_level_schema";
const selector = 'assert_eq!(value["kind"].as_str(), Some("a2a_status"));';

const docsRegex =
  /- `kind`: literal string `"a2a_status"`\. Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("a2a_status"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "a2a_status kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"a2a_status"`. Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("a2a_status")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the a2a_status pins-test still exists under its symmetric fn name a2a_status_json_pins_top_level_schema`,
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
    "validate-a2a-status-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
