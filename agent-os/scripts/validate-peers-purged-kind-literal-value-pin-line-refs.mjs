#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peers_purged kind literal value pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 313 cites a main.rs line for
// the value-level pin on
// `value["kind"].as_str() == Some("peers_purged")` inside
// `peers_purge_json_pins_top_level_schema`. Note the
// verb-vs-past-tense asymmetry (same family as capabilities_purged
// and capability_granted / capability_revoked): the envelope's kind
// literal is the past-tense `peers_purged` (the outcome name) but
// the test fn name uses the verb form
// `peers_purge_json_pins_top_level_schema` (mirroring the emitter
// helper `peers_purge_json`).
//
// Selector form: the single-line statement
// `assert_eq!(value["kind"].as_str(), Some("peers_purged"));`
// appears exactly once in main.rs (line 6141). The
// `"peers_purged"` literal in the selector and the brace-scoping
// to peers_purge_json_pins_top_level_schema together isolate this
// match.
//
// Cross-line collision notes:
//   * docs/ipc-and-http-gateway.md line 282 (the capabilities_purged
//     bullet) mentions `*_purged` in glob form but never uses the
//     double-quoted `"peers_purged"` literal, so the
//     `` `"peers_purged"` `` anchor below cannot match it.
//   * docs/ipc-and-http-gateway.md line 334 (the peer_revoke
//     bullet) lists `` `peers_purged` `` in single-backtick form
//     inside its past-tense-sibling disambiguator prose, but again
//     never uses the double-quoted literal.
// Both bullets are therefore safe siblings under the anchor below.
//
// Bullet-line shape: peers_purged's bullet carries an inline
// "structural disambiguator from capabilities_purged" note between
// the literal and the Pinned-at sentence, so the docsRegex uses
// `[^\n]*` to bridge the gap. Both ends of the bridge are uniquely
// anchored: the opening literal `"peers_purged"` and the closing
// `Some("peers_purged")` cite. Single-line cite pattern; the docs
// cite is `main.rs:N`.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "peers_purge_json_pins_top_level_schema";
const selector =
  'assert_eq!(value["kind"].as_str(), Some("peers_purged"));';

const docsRegex =
  /- `kind`: literal string `"peers_purged"`[^\n]*Pinned at the value level by `main\.rs:(\d+)` \(asserts `value\["kind"\]\.as_str\(\) == Some\("peers_purged"\)`\), so a future kind-rename fails the test rather than silently rewriting the discriminator string\./;
const docsLabel = "peers_purged kind literal value pin citation";
const docsTemplate =
  '- `kind`: literal string `"peers_purged"` … Pinned at the value level by `main.rs:N` (asserts `value["kind"].as_str() == Some("peers_purged")`), so a future kind-rename fails the test rather than silently rewriting the discriminator string.';

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the peers_purged pins-test still exists under the verb-form fn name peers_purge_json_pins_top_level_schema (envelope literal is past-tense but the test fn name is the verb form)`,
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
    "validate-peers-purged-kind-literal-value-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peers-purged-kind-literal-value-pin-line-refs: ok (kind-value main.rs:${selectorLine})`,
);
