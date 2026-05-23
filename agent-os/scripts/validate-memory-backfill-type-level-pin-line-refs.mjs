#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory_backfill envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two inner assertion ranges inside
// memory_backfill_json_pins_top_level_schema:
//
//   - line 650 cites `row_count` (u64) at main.rs:5637-5640.
//   - line 651 cites `savepoint_name` (string) at main.rs:5641-5644.
//
// The row_count cite landed first as a single-target validator; the
// savepoint_name cite was added later when the validator was converted
// to the multi-target shape (mirroring validate-bootstrap-result and
// validate-tool-result conversions).
//
// Sibling collision risk (mirrors validate-settlement-backfill):
// the settlement_backfill envelope's pins test at main.rs:5550 carries
// the same `value["row_count"].is_u64(),` selector at main.rs:5575
// and a near-identical row_count docs bullet at line 635. The
// savepoint_name selector and bullet are currently unique to
// memory_backfill (settlement uses rollback_path with a string-or-null
// shape instead). Both risks are addressed:
//
//   - Selector lookups scope to the brace-balanced
//     `memory_backfill_json_pins_top_level_schema` fn body, so the
//     settlement_backfill row_count occurrence at main.rs:5575 cannot
//     contaminate the result.
//   - The row_count docsRegex anchors on the memory-specific phrase
//     "memory records the correlation pass operated on", and the
//     savepoint_name docsRegex anchors on "SQLite SAVEPOINT identifier
//     the daemon emitted for this pass". Neither phrase appears in the
//     settlement_backfill bullets, so first-match capture cannot drift.
//
// Each range is derived as assert!-opener-to-closer (4-line convention)
// — the cite spans the `assert!(` opener directly above the selector
// through the closing `);` on its own line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "memory_backfill_json_pins_top_level_schema";

const targets = [
  {
    field: "row_count",
    selector: 'value["row_count"].is_u64(),',
    docsRegex:
      /- `row_count` \(u64\): count of memory records the correlation pass operated on \(mutation path\) or \*would\* operate on \(dry-run path\)\. May legitimately be `0` when no legacy rows match\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string-of-integer\./,
    docsLabel: "memory_backfill.row_count type-level pin citation",
    docsTemplate:
      "Pinned as u64 by `main.rs:N-M` — never a string-of-integer.",
  },
  {
    field: "savepoint_name",
    selector: 'value["savepoint_name"].is_string(),',
    docsRegex:
      /- `savepoint_name` \(string\): SQLite SAVEPOINT identifier the daemon emitted for this pass\. \*\*Always a non-null string\*\* — the field type at `memory_backfill_json` \(`main\.rs:\d+`\) is `&str`, not `Option<&str>`, so even a dry-run call returns a real savepoint name \(the daemon allocates one so consumers can correlate planning runs against later mutation runs\)\. JSON consumers must not write null-vs-value branching for this field; treat absence as a protocol violation\. This is the only field-shape difference from `settlement\.backfill\.v1`, whose sibling `rollback_path` is string-or-null\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never null \(the &str emitter type forbids null at compile time\)\./,
    docsLabel: "memory_backfill.savepoint_name type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never null (the &str emitter type forbids null at compile time).",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the memory_backfill pins-test still exists and is not renamed or duplicated`,
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
  console.error("validate-memory-backfill-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
