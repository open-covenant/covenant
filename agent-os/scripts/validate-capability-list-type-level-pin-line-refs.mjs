#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_list envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two inner assertion ranges
// inside capability_list_json_pins_top_level_schema:
//
//   - line 248 cites `limit` (u64) type pin at :5919-5922.
//   - line 249 cites `capabilities` (array) type pin at :5923-5926.
//
// Both cites use the 4-line assert!-opener-to-closer range convention
// that sibling envelopes already use (audit_verify at line 384
// :6478-6481, receipt_list at lines 198/199, peer_list at line 298,
// capability_grant at line 261 :5996-5999). The range convention
// catches a wider set of drift modes: a single-line cite stays correct
// when the assert!( opener silently moves up/down, while the 4-line
// range cite fails loudly.
//
// The validator scopes each lookup to the brace-balanced
// `capability_list_json_pins_top_level_schema` fn body so a same-named
// selector inside a different envelope's pins test (receipt_list,
// peer_list, intent_result, a2a_status, audit_recent, memory_read for
// the same `value["limit"].is_u64()` selector; bootstrap_result for the
// `value["capabilities"]`-ish array shape) cannot contaminate the
// result.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "capability_list_json_pins_top_level_schema";

const targets = [
  {
    field: "limit",
    selector: 'value["limit"].is_u64(),',
    docsRegex:
      /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, see `main\.rs:\d+`\)\. Pinned at the type level by the schema test \(`main\.rs:(\d+)-(\d+)`\) — JSON consumers must never receive a string here\./,
    docsLabel: "capability_list.limit type-level pin citation",
    docsTemplate:
      "Pinned at the type level by the schema test (`main.rs:N-M`) — JSON consumers must never receive a string here.",
  },
  {
    field: "capabilities",
    selector: 'value["capabilities"].is_array(),',
    docsRegex:
      /- `capabilities` \(array of `SignedCapability`\): the filtered live capabilities\. Each element has shape `\{capability: Capability, signature: <base58>\}` where `Capability` is defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:\d+` \(fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`\) and `SignedCapability` is defined at `agent-os\/crates\/covenant-permissions\/src\/lib\.rs:\d+`\. The `signature` field is the base58 encoding of the 64-byte ed25519 signature \(per the `sig_b58` serde module at `lib\.rs:\d+-\d+`\), never the raw byte array\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "capability_list.capabilities type-level pin citation",
    docsTemplate:
      "Pinned as an array by `main.rs:N-M` — never null or a string.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the capability_list pins-test still exists and is not renamed or duplicated`,
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
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector`,
          );
          continue;
        }
        const startLine = assertOpenerLine;
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
        target.startLine = startLine;
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
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} type-level pin line range`,
      );
      continue;
    }
    if (target.startLine !== undefined && target.endLine !== undefined) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${citedStart}-${citedEnd} but the ${target.field} type-level assertion spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-capability-list-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-list-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
