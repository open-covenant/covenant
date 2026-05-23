#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_grant envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites four main.rs ranges inside
// capability_grant_json_pins_top_level_schema:
//
//   - line 261 cites the `action` (string) type pin at :5996-5999.
//   - line 262 cites the `signature_b58` (string) type pin at
//     :6000-6003.
//   - line 263 cites the `scope` (object or null) type pin at
//     :6004-6007.
//   - line 264 cites the `expires_at` (u64 or null) type pin at
//     :6008-6011.
//
// The cites use the 4-line assert!-opener-to-closer range convention
// that sibling envelopes already use (audit_verify at line 384
// :6478-6481, capability_list at line 248 :5919-5922). The range
// convention catches a wider set of drift modes: a single-line cite
// stays correct when the assert!( opener silently moves up/down,
// while the 4-line range cite fails loudly.
//
// The validator scopes each lookup to the brace-balanced
// `capability_grant_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test cannot contaminate
// the result.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "capability_grant_json_pins_top_level_schema";

const targets = [
  {
    field: "action",
    selector: 'value["action"].is_string(),',
    docsRegex:
      /- `action` \(string\): the action the capability was granted for\. \*\*Not always the verbatim CLI argument\*\*: when the CLI receives an a2a peer-prefix shorthand it expands the prefix to the full peer-bound action before signing \(see `expand_a2a_action` invoked at `main\.rs:\d+-\d+`\); the envelope reports the post-expansion full form, and the unsuffixed CLI prints an `expanding <prefix> → <full>` line to stderr at `main\.rs:\d+`\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never an object or array\./,
    docsLabel: "capability_grant.action type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never an object or array.",
  },
  {
    field: "signature_b58",
    selector: 'value["signature_b58"].is_string(),',
    docsRegex:
      /- `signature_b58` \(string\): the base58 signature over the signed-capability bytes\. This is the same value consumers pass back to `covenant capabilities revoke <signature-b58>` to tombstone the capability\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never an object or array\./,
    docsLabel: "capability_grant.signature_b58 type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never an object or array.",
  },
  {
    field: "scope",
    selector: 'value["scope"].is_object() || value["scope"].is_null(),',
    docsRegex:
      /- `scope` \(object or null\): the structured scope object echoed from the request, or `null` when `--scope` was omitted\. Pinned at the type level by the schema test \(`main\.rs:(\d+)-(\d+)`\) — JSON consumers must never receive a string blob here, so a scope value of `"\{\\"version\\":1\}"` would be a contract break\./,
    docsLabel: "capability_grant.scope type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N-M`) — JSON consumers must never receive a string blob here",
  },
  {
    field: "expires_at",
    selector:
      'value["expires_at"].is_u64() || value["expires_at"].is_null(),',
    docsRegex:
      /- `expires_at` \(u64 or null\): the Unix-epoch millisecond expiry echoed from `--expires-at`, or `null` when the flag was omitted\. Pinned at the type level by the schema test \(`main\.rs:(\d+)-(\d+)`\) — JSON consumers must never receive a string here, so a value of `"1700000000000"` would be a contract break\./,
    docsLabel: "capability_grant.expires_at type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N-M`) — JSON consumers must never receive a string here",
  },
];

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the capability_grant pins-test still exists and is not renamed or duplicated`,
    );
  } else {
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
    } else {
      for (const target of targets) {
        const selectorMatches = [];
        for (let index = testStart; index < testEnd; index += 1) {
          if (lines[index].trim() === target.selector) {
            selectorMatches.push(index + 1);
          }
        }
        if (selectorMatches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion is present exactly once in this test`,
          );
          continue;
        }
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-to-closing convention requires the assert!( opener on the line directly above the selector`,
          );
          continue;
        }
        target.startLine = assertOpenerLine;
        let endLine = null;
        for (let index = selectorLine; index < testEnd; index += 1) {
          if (lines[index].trim() === ");") {
            endLine = index + 1;
            break;
          }
        }
        if (endLine === null) {
          fail(
            `${sourcePath}: could not find the closing \`);\` after the ${target.field} selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
          );
          continue;
        }
        target.endLine = endLine;
      }
    }
  }
}

if (docs) {
  for (const target of targets) {
    const match = docs.match(target.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} type-level pin line range as the modern :N-M form (not the single-line :N form)`,
      );
      continue;
    }
    if (target.startLine !== undefined && target.endLine !== undefined) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${citedStart}-${citedEnd} but the capability_grant.${target.field} type-level assertion spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-capability-grant-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-grant-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
