#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// audit_integrity kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 383 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("audit_integrity")` inside
// `audit_verify_json_pins_top_level_schema`. This slice carries
// the most extreme verb-vs-outcome name asymmetry in the
// kind-literal-value pin campaign:
//   * helper fn:   audit_verify_json            (verb form)
//   * test fn:     audit_verify_json_pins_top_level_schema
//                                                (verb form)
//   * envelope:    "audit_integrity"            (noun/outcome
//                                                name, unrelated
//                                                to the verb verify)
// A validator that derives the test fn name from the envelope
// literal (audit_integrity_json_pins_top_level_schema) would
// falsely report missing; this script hard-codes both names
// independently.
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("audit_integrity"));`
// appears exactly once in main.rs (line 6477). The
// `"audit_integrity"` literal in the selector and the
// brace-scoping to audit_verify_json_pins_top_level_schema
// together isolate this match.
//
// Cross-line collision notes:
//   * The audit_integrity bullet's own disambiguator prose
//     mentions both `` `verify` `` and `` `verify_report` `` in
//     single-backtick form, but never the
//     `` `"audit_integrity"` `` double-quoted form anywhere else.
//   * The workspace-level verify_report bullet at line 115 does
//     not contain the audit_integrity literal in any form.
// The `` `"audit_integrity"` `` anchor below is therefore safe.
//
// Bullet-line shape: audit_integrity's bullet carries inline
// disambiguator prose between the literal and the Pinned-at
// sentence, so the docsRegex uses `[^\n]*` to bridge the gap.
// Both ends of the bridge are uniquely anchored: the opening
// literal `"audit_integrity"` and the closing
// `Some("audit_integrity")` cite. Single-line cite pattern; the
// docs cite is `main.rs:N`.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "audit_verify_json_pins_top_level_schema";
const selector =
  'assert_eq!(value["kind"].as_str(), Some("audit_integrity"));';

const docsRegex =
  /- `kind`: literal string `"audit_integrity"`[^\n]*Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("audit_integrity"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "audit_integrity kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"audit_integrity"` … Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("audit_integrity")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the audit_integrity pins-test still exists under the verb-form fn name audit_verify_json_pins_top_level_schema (envelope literal is the noun audit_integrity but the test fn name uses the verb verify)`,
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
    "validate-audit-integrity-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-audit-integrity-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
