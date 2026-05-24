#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peer_list peers type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 296 cites a main.rs line for the
// type-level pin on `value["peers"].is_array()` inside
// `peer_list_json_pins_top_level_schema`. The assertion is single-
// line — `assert!(value["peers"].is_array(), "peers must be an
// array: {value}",);` — with a trailing comma before the closing
// paren that the selector must reproduce exactly.
//
// Selector form: the single-line assertion is unique inside main.rs
// (one occurrence at line 5061). Brace-scoping to peer_list_json_
// pins_top_level_schema isolates the match defensively.
//
// docsRegex anchoring: the peer_list envelope's adjacent bullets at
// lines 295 (matched_count), 297 (operator_pubkey_b58), and 298
// (truncated) all carry "Pinned as ..." wording with their own
// ranges. The regex anchors on the `- \`peers\` (array of
// \`PeerSummary\`)` bullet head plus the unique "the matched roster
// slice, see below." trailer to isolate this bullet from its
// siblings.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "peer_list_json_pins_top_level_schema";
const selector =
  'assert!(value["peers"].is_array(), "peers must be an array: {value}",);';

const docsRegex =
  /- `peers` \(array of `PeerSummary`\): the matched roster slice, see below\. Pinned as an array by `main\.rs:(\d+)` — never null or a string blob\./;
const docsLabel = "peer_list.peers type-level pin citation";
const docsTemplate =
  '- `peers` (array of `PeerSummary`): the matched roster slice, see below. Pinned as an array by `main.rs:N` — never null or a string blob.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the peer_list pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the peers type-level assertion is present exactly once in this test`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the peers type-level pin line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the peers type-level assertion lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-peer-list-peers-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-list-peers-type-level-pin-line-refs: ok (peers main.rs:${selectorLine})`,
);
