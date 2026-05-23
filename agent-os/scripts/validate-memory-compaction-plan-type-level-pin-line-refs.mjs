#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_compaction_plan envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two assert_eq! ranges inside
// memory_compaction_plan_json_pins_expected_receipt_changes_schema:
//
//   - line 568 cites `mode` (string == "none") at main.rs:6721-6725.
//   - line 569 cites `records` (Vec::len() == 0) at main.rs:6730-6736.
//
// Both cites use the assert_eq!-opener-to-closer range convention
// (5-line shape for the mode block, 7-line shape for the records block
// because the latter's selector spans 3 lines before the expected
// value). This convention is the assert_eq! analogue of the assert!-
// opener-to-closer convention used by the sibling type-pin validators.
//
// The mode selector value["expected_receipt_changes"]["mode"].as_str()
// is unique to the assert_eq! body in the entire file. The records
// selector value["expected_receipt_changes"]["records"] (with no
// suffix) appears twice inside this test fn — line 6727 has it with
// `.is_array(),` appended on the same line (different is_array
// assertion), and line 6731 has it alone (this assert_eq!'s first
// body line). Exact-trim-match plus brace-balanced fn body scoping
// isolates the assert_eq!-body occurrence cleanly.

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
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} value-pin assertion is present exactly once in this test`,
          );
          continue;
        }
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert_eq!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert_eq!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert_eq!-to-closing convention requires the assert_eq!( opener on the line directly above the selector`,
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
            `${sourcePath}: could not find the closing \`);\` after the ${target.field} selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
        `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.field} value-pin line range`,
      );
      continue;
    }
    if (target.startLine !== undefined && target.endLine !== undefined) {
      const citedStart = parseInt(match[1], 10);
      const citedEnd = parseInt(match[2], 10);
      if (citedStart !== target.startLine || citedEnd !== target.endLine) {
        fail(
          `${docsPath}: the ${target.docsLabel} cites main.rs:${citedStart}-${citedEnd} but the ${target.field} value-pin assertion spans :${target.startLine}-${target.endLine}; remediation: update the citation to :${target.startLine}-${target.endLine}`,
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
