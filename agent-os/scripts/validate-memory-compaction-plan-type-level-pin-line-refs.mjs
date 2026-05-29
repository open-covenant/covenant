#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_compaction_plan envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites five inner ranges inside
// memory_compaction_plan_json_pins_expected_receipt_changes_schema:
//
//   - line 568 cites `mode` (string == "none") at the assert_eq!
//     range that pins the literal value, plus `mode.is_string()` at
//     the assert! range that pins the type — two cites on the same
//     bullet, distinguished by docsRegex anchors.
//   - line 569 cites `records` (Vec::len() == 0) at the assert_eq!
//     range that pins the empty length, plus `records.is_array()` at
//     the assert! range that pins the type — two cites on the same
//     bullet, distinguished by docsRegex anchors.
//   - line 570 cites `reason` (is_string()) at the assert! range
//     that pins the type — the reason field has no value pin because
//     the docs explicitly call out that consumers must not branch on
//     the exact text.
//
// Mixed opener conventions: mode/records use assert_eq!( (value pins)
// and reason uses assert!( (type pin). Per-target `opener` field
// selects between them; default is "assert_eq!" so the prior targets
// keep their existing behavior.
//
// All three cites use the opener-to-closer range convention: the cite
// spans from the line containing the opener through the closing `);`
// on its own line. The closing-`);`-on-own-line invariant lets the
// validator find the range end without parsing macro argument lists.
//
// Selector collision notes: mode's selector is unique to the
// assert_eq! body in the entire file. The records selector
// value["expected_receipt_changes"]["records"] (with no suffix)
// appears twice inside this test fn — line 6727 has it with
// `.is_array(),` appended on the same line (different is_array
// assertion), and line 6731 has it alone (this assert_eq!'s first
// body line). Exact-trim-match plus brace-balanced fn body scoping
// isolates the assert_eq!-body occurrence cleanly. The reason
// selector value["expected_receipt_changes"]["reason"].is_string()
// is currently unique to this test fn.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName =
  "memory_compaction_plan_json_pins_expected_receipt_changes_schema";

const targets = [
  {
    field: "mode",
    selector: 'value["expected_receipt_changes"]["mode"].as_str(),',
    docsRegex:
      /- `mode` \(string\): literal `"none"` today\. Pinned by the schema test at `main\.rs:(\d+)-(\d+)` as the only currently-allowed value; consumers must treat any other value as a sign that receipt-aware compaction has shipped and the docs are stale\./,
    docsLabel: "memory_compaction_plan.mode value pin citation",
    docsTemplate:
      "Pinned by the schema test at `main.rs:N-M` as the only currently-allowed value",
  },
  {
    field: "records",
    selector: 'value["expected_receipt_changes"]["records"]',
    docsRegex:
      /- `records` \(array\): empty today \(length pinned to `0` at `main\.rs:(\d+)-(\d+)`\)\. Will gain a real shape once receipt-aware compaction lands\./,
    docsLabel: "memory_compaction_plan.records length pin citation",
    docsTemplate: "length pinned to `0` at `main.rs:N-M`",
  },
  {
    field: "mode_type",
    selector: 'value["expected_receipt_changes"]["mode"].is_string(),',
    opener: "assert!",
    docsRegex:
      /receipt-aware compaction has shipped and the docs are stale\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never a structured object\./,
    docsLabel: "memory_compaction_plan.mode type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never a structured object.",
  },
  {
    field: "records_type",
    selector: 'value["expected_receipt_changes"]["records"].is_array(),',
    opener: "assert!",
    docsRegex:
      /Will gain a real shape once receipt-aware compaction lands\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "memory_compaction_plan.records type-level pin citation",
    docsTemplate:
      "Pinned as an array by `main.rs:N-M` — never null or a string.",
  },
  {
    field: "reason",
    selector: 'value["expected_receipt_changes"]["reason"].is_string(),',
    opener: "assert!",
    docsRegex:
      /- `reason` \(string\): a human-readable explanation of why the block is empty\. Currently the literal `"dry-run compaction planning does not mutate memory or settlement receipts"` per `main\.rs:\d+`; consumers must not branch on the exact text — only on the field's existence and type\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never a structured object\./,
    docsLabel: "memory_compaction_plan.reason type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never a structured object.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_compaction_plan expected_receipt_changes pins-test still exists and is not renamed or duplicated`,
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
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} pin assertion is present exactly once in this test`,
          );
          continue;
        }
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        const expectedOpener = `${target.opener ?? "assert_eq!"}(`;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== expectedOpener
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`${expectedOpener}\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the ${expectedOpener.replace("(", "")}-to-closing convention requires the ${expectedOpener} opener on the line directly above the selector`,
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
            `${sourcePath}: could not find the closing \`);\` after the ${target.field} selector at line ${selectorLine}; remediation: confirm the surrounding ${expectedOpener.replace("(", "")} macro is closed on its own line`,
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
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} pin line range`,
      );
      continue;
    }
    if (target.startLine !== undefined && target.endLine !== undefined) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${citedStart}-${citedEnd} but the ${target.field} pin assertion spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-memory-compaction-plan-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-compaction-plan-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
