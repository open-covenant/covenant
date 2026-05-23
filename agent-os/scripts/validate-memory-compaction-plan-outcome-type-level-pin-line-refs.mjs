#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_compaction_plan.outcome envelope type-level pin line-ref
// drift guard. docs/ipc-and-http-gateway.md line 561 cites a single
// main.rs range for the outcome (structured object) type pin inside
// the memory_compaction_plan_json_pins_top_level_schema fn body.
//
// Naming note: distinct from validate-memory-compaction-plan-
// type-level-pin-line-refs.mjs, which despite its general-sounding
// name actually covers value-level pins (mode == "none", records
// length == 0) inside the *other* pin test
// memory_compaction_plan_json_pins_expected_receipt_changes_schema
// (assert_eq! convention). This validator covers the *type-level*
// pin from the top_level_schema test (assert! convention).
//
// Sibling collision risk: the `value["outcome"].is_object(),` selector
// appears in two other pin tests in main.rs:
//
//   - peer_revoke_json_pins_top_level_schema at main.rs:5166
//     (peer_revoke.outcome type pin).
//   - memory_compaction_json_pins_top_level_schema at main.rs:6602
//     (memory_compaction.outcome type pin).
//
// Both risks are addressed by scoping the selector lookup to the
// brace-balanced `memory_compaction_plan_json_pins_top_level_schema`
// fn body — the sibling selectors live in different test fns and
// cannot contaminate the result. The docsRegex anchors on the
// memory_compaction_plan-specific phrase "For this verb,
// `outcome.mode` is **always** `\"dry_run\"`", which neither the
// peer_revoke nor memory_compaction outcome bullets contain (the
// memory_compacted bullet references mode without the dry-run
// invariant since that envelope supports both apply and dry-run
// modes; the peer_revoke bullet covers a tagged-enum outcome).
//
// The range is derived as assert!-opener-to-closer (4-line
// convention) — the cite spans the `assert!(` opener directly above
// the selector through the closing `);` on its own line, mirroring
// validate-chain-status-type-level-pin-line-refs.mjs and
// validate-ping-type-level-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "memory_compaction_plan_json_pins_top_level_schema";
const selector = 'value["outcome"].is_object(),';

const docsRegex =
  /- `outcome` \(object\): the same `MemoryCompactionOutcome` shape documented in the `memory_compacted` block above\. For this verb, `outcome\.mode` is \*\*always\*\* `"dry_run"` and `outcome\.changed` is \*\*always\*\* `false`; a non-`dry_run` value here indicates daemon\/CLI drift and JSON consumers should treat it as a protocol violation\. Pinned as a structured object by `main\.rs:(\d+)-(\d+)` — never a string blob\./;
const docsLabel = "memory_compaction_plan.outcome type-level pin citation";
const docsTemplate =
  "Pinned as a structured object by `main.rs:N-M` — never a string blob.";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_compaction_plan pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the outcome type-level assertion is present exactly once in this test`,
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
              `${sourcePath}: could not find the closing \`);\` after the outcome selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the outcome type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the outcome type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-compaction-plan-outcome-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compaction-plan-outcome-type-level-pin-line-refs: ok (memory_compaction_plan.outcome main.rs:${startLine}-${endLine})`,
);
