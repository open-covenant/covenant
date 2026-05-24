#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_compacted kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 531 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("a2a_compacted")` inside
// `a2a_compact_json_pins_top_level_schema`. Three-way name
// asymmetry (same shape as memory_compacted):
//   * envelope:    "a2a_compacted"           (past-tense outcome)
//   * helper fn:   a2a_compact_json          (verb form)
//   * test fn:     a2a_compact_json_pins_top_level_schema
//                                             (verb form)
// A validator that derives the test fn name from the envelope
// literal (a2a_compacted_json_pins_top_level_schema) would falsely
// report missing; this script hard-codes both names independently.
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("a2a_compacted"));`
// appears exactly once in main.rs (line 6215). The
// `"a2a_compacted"` literal in the selector and the brace-scoping
// to a2a_compact_json_pins_top_level_schema together isolate this
// match.
//
// Cross-line collision notes:
//   * The a2a_compacted bullet's own disambiguator prose mentions
//     `` `"a2a_compact"` `` and `` `"a2a_compaction"` `` as
//     strawman tokens consumers might guess. Neither of those
//     quoted strings ends in the `"a2a_compacted"` literal, so the
//     `` `"a2a_compacted"` `` anchor below cannot match them.
//   * The memory_compacted bullet at line 540 is a structurally
//     similar past-tense outcome envelope, but its kind literal is
//     `"memory_compacted"` (different anchor) and its closing
//     cite is `Some("memory_compacted")`, so cross-bullet capture
//     is impossible.
// The `` `"a2a_compacted"` `` anchor below is therefore safe.
//
// Bullet-line shape: a2a_compacted's bullet carries inline
// disambiguator prose between the literal and the Pinned-at
// sentence, so the docsRegex uses `[^\n]*` to bridge the gap
// (same approach as memory_compacted, capability_list,
// peer_revoke, receipt_list, tool_list, tool_result). Both ends
// of the bridge are uniquely anchored: the opening literal
// `"a2a_compacted"` and the closing `Some("a2a_compacted")` cite.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "a2a_compact_json_pins_top_level_schema";
const selector = 'assert_eq!(value["kind"].as_str(), Some("a2a_compacted"));';

const docsRegex =
  /- `kind`: literal string `"a2a_compacted"`[^\n]*Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("a2a_compacted"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "a2a_compacted kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"a2a_compacted"` … Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("a2a_compacted")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the a2a_compacted pins-test still exists under its asymmetric fn name a2a_compact_json_pins_top_level_schema (helper fn is a2a_compact_json, envelope literal is a2a_compacted)`,
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
    "validate-a2a-compacted-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-compacted-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
