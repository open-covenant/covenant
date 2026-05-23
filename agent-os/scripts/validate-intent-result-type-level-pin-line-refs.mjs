#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intent_result envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites three inner assertion ranges
// inside intent_result_json_pins_top_level_schema:
//
//   - line 235 cites `intent_id` (string) type pin.
//   - line 236 cites `status` (string) type pin.
//   - line 239 cites `settlement` (object or null) type pin.
//
// The existing validate-intent-result-line-refs.mjs covers the helper
// fn, renders test, and pins test declaration lines, but not the
// inner type-level selector ranges. The intent_id and status cites
// were stale by ~222 lines before this slice (they pointed into the
// intents_resume_ok_json_pins_top_level_schema body). The settlement
// cite was already current.
//
// The validator scopes each lookup to the brace-balanced
// `intent_result_json_pins_top_level_schema` fn body so the same
// selectors inside intents_resume_ok/intents_resume_error/sibling tests
// cannot contaminate the result. Each target declares its own range
// convention: intent_id and status use assert!-opener-to-closer (the
// 4-line shape preserved from their original cites), while settlement
// uses selector-to-closer (the 3-line shape preserved from its
// original cite).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "intent_result_json_pins_top_level_schema";

const targets = [
  {
    field: "intent_id",
    selector: 'value["intent_id"].is_string(),',
    convention: "assert-opener",
    docsRegex:
      /- `intent_id` \(string\): the dispatched intent's UUID, serialized as the canonical hyphenated string form\. Pinned as a string by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never a byte array or struct\./,
    docsLabel: "intent_result.intent_id type-level pin citation",
    docsTemplate:
      "Pinned as a string by the schema test (`main.rs:N-M`) — never a byte array or struct.",
  },
  {
    field: "status",
    selector: 'value["status"].is_string(),',
    convention: "assert-opener",
    docsRegex:
      /- `status` \(string\): the outcome status \(e\.g\., `"ok"`\)\. The string shape is pinned by `main\.rs:(\d+)-(\d+)`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface\./,
    docsLabel: "intent_result.status type-level pin citation",
    docsTemplate:
      "The string shape is pinned by `main.rs:N-M`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.",
  },
  {
    field: "settlement",
    selector:
      'value["settlement"].is_object() || value["settlement"].is_null(),',
    convention: "selector",
    docsRegex:
      /- `settlement` \(object or null\): an optional `SettlementReceipt` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:\d+`\) carrying the on-chain or local settlement evidence when the intent consumed credits\. `null` when the intent did not settle \(e\.g\., a phase-0 echo that does not charge\)\. Pinned as object-or-null by `main\.rs:(\d+)-(\d+)` — never an integer or array\./,
    docsLabel: "intent_result.settlement type-level pin citation",
    docsTemplate:
      "Pinned as object-or-null by `main.rs:N-M` — never an integer or array.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the intent_result pins-test still exists and is not renamed or duplicated`,
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
        let startLine;
        if (target.convention === "assert-opener") {
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
          startLine = assertOpenerLine;
        } else {
          startLine = selectorLine;
        }
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
  console.error("validate-intent-result-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intent-result-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
