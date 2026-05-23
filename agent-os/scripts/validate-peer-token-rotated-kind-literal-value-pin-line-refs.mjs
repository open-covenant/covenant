#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peer_token_rotated kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 323 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("peer_token_rotated")` inside
// `peers_rotate_json_pins_top_level_schema`. Two name asymmetries
// stack here (more than the usual single asymmetry in the
// capabilities_purged / capability_granted siblings):
//   1. Past-tense outcome vs verb test fn: the envelope's kind
//      literal is the past-tense `peer_token_rotated` (the outcome
//      name) but the test fn uses the verb form
//      `peers_rotate_json_pins_top_level_schema`.
//   2. Singular envelope vs plural emitter/test: the envelope
//      literal is singular `peer_token_rotated` while the helper
//      fn is plural `peers_rotate_json` and so is the test fn
//      (`peers_rotate_json_pins_top_level_schema`). A validator
//      that searches for a singular `peer_rotate_*` or past-tense
//      `peer_token_rotated_*` fn name would falsely report missing.
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("peer_token_rotated"));`
// appears exactly once in main.rs (line 6180). The
// `"peer_token_rotated"` literal in the selector and the
// brace-scoping to peers_rotate_json_pins_top_level_schema
// together isolate this match.
//
// Cross-line collision note: docs/ipc-and-http-gateway.md line 334
// (the peer_revoke bullet) lists `` `peer_token_rotated` `` in
// single-backtick form inside its past-tense-sibling disambiguator
// prose, but never uses the double-quoted literal. The
// `` `"peer_token_rotated"` `` anchor below therefore cannot match
// it.
//
// Bullet-line shape: peer_token_rotated's bullet has no extra
// inline prose between the literal and the Pinned-at sentence
// (clean bullet, period separator like bootstrap_result,
// daemon_ping, and capabilities_purged), so the docsRegex uses the
// strict `. Pinned` join rather than the permissive `[^\n]*` bridge
// used by sibling validators with mid-line prose.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "peers_rotate_json_pins_top_level_schema";
const selector =
  'assert_eq!(value["kind"].as_str(), Some("peer_token_rotated"));';

const docsRegex =
  /- `kind`: literal string `"peer_token_rotated"`\. Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("peer_token_rotated"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "peer_token_rotated kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"peer_token_rotated"`. Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("peer_token_rotated")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the peer_token_rotated pins-test still exists under the verb-form, plural fn name peers_rotate_json_pins_top_level_schema (envelope literal is past-tense singular peer_token_rotated but the test fn name is verb-form plural)`,
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
    "validate-peer-token-rotated-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-token-rotated-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
