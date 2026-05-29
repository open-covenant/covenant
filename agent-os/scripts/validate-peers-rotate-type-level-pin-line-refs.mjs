#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peers_rotate envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 324 cites a single main.rs range
// for the peers_rotate.token_b58 (string) type pin inside the
// peers_rotate_json_pins_top_level_schema fn body.
//
// Before this validator landed, the docs prose described `token_b58`
// as "(string)" with secret-bearing warning prose but no main.rs cite.
// The assertion at main.rs:6181-6184 (assert!(value["token_b58"].is_string(),
// ...);) was only enforced at test runtime. The validator binds the
// docs prose to the source-of-truth so a regression that turned the
// field into a byte array (e.g., raw 32-byte ed25519 token) or a
// structured object (e.g., {token: ..., expires_at: ...}) surfaces at
// the docs-validator level (not just at test runtime). Both regressions
// would silently break credential-rotation consumers since the daemon
// continues to authenticate using the persisted file, but the wire
// envelope would mismatch the documented schema.
//
// The validator scopes its lookup to the brace-balanced
// `peers_rotate_json_pins_top_level_schema` fn body. The
// value["token_b58"].is_string(), selector is currently unique inside
// main.rs (this is the only peers_rotate envelope assertion using that
// exact form), but the brace-scoping plus exact-trim-match also
// isolates the peers_rotate one if a sibling envelope later adds the
// same selector. The range is derived as assert!-opener-to-closer
// (4-line convention) — the cite spans the `assert!(` opener directly
// above the selector through the closing `);` on its own line,
// mirroring validate-tool-list-type-level-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "peers_rotate_json_pins_top_level_schema";
const selector = 'value["token_b58"].is_string(),';

const docsRegex =
  /- `token_b58` \(string\): the full base58 operator token\. The value is the new authentication credential, not a fingerprint — the envelope is \*\*secret-bearing\*\* and JSON output must be treated as sensitive \(no logging, no shell history capture, no transport over unsecured channels\)\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never bytes or a structured object\./;
const docsLabel = "peers_rotate.token_b58 type-level pin citation";
const docsTemplate =
  "Pinned as a string by `main.rs:N-M` — never bytes or a structured object.";

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

let startLine = null;
let endLine = null;
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the peers_rotate pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the token_b58 type-level assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector`,
          );
        } else {
          startLine = assertOpenerLine;
          for (let index = selectorLine; index < testEnd; index += 1) {
            if (lines[index].trim() === ");") {
              endLine = index + 1;
              break;
            }
          }
          if (endLine === null) {
            fail(
              `${sourcePath}: could not find the closing \`);\` after the token_b58 selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
            );
          }
        }
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the token_b58 type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the token_b58 type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-peers-rotate-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peers-rotate-type-level-pin-line-refs: ok (peers_rotate.token_b58 main.rs:${startLine}-${endLine})`,
);
