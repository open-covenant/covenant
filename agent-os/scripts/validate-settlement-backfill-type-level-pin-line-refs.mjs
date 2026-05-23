#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement_backfill envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites three inner assertion ranges
// inside settlement_backfill_json_pins_top_level_schema:
//
//   - line 634 cites `schema` (string) at main.rs:5565-5568.
//   - line 635 cites `row_count` (u64) at main.rs:5574-5577.
//   - line 636 cites `rollback_path` (string or null) at
//     main.rs:5578-5581.
//
// The row_count cite landed first as a single-target validator; the
// rollback_path cite was added later when the validator was converted
// to the multi-target shape (mirroring just-landed memory_backfill
// conversion).
//
// Sibling collision risk (mirrors validate-memory-backfill): the
// memory_backfill envelope's pins test at main.rs:5613 carries the
// same `value["row_count"].is_u64(),` selector at main.rs:5638 and
// the same `value["schema"].is_string(),` selector at main.rs:5629,
// plus a near-identical row_count docs bullet at line 650 and a
// schema docs bullet at line 649 that references the
// settlement.backfill.v1 literal by name ("Same versioning semantics
// as `covenant.settlement.backfill.v1`"). The rollback_path selector
// and bullet are currently unique to settlement_backfill (memory uses
// savepoint_name with a non-nullable shape instead). All three risks
// are addressed:
//
//   - Selector lookups scope to the brace-balanced
//     `settlement_backfill_json_pins_top_level_schema` fn body, so the
//     memory_backfill row_count occurrence at main.rs:5638 and the
//     memory_backfill schema occurrence at main.rs:5629 cannot
//     contaminate the result.
//   - The row_count docsRegex anchors on the settlement-specific
//     phrase "legacy settlement-receipt rows the backfill operated on"
//     plus the closer "the verb does not error on an empty backfill".
//   - The rollback_path docsRegex anchors on "filesystem path to the
//     rollback-evidence file written by a mutation pass".
//   - The schema docsRegex anchors on "literal string
//     `\"covenant.settlement.backfill.v1\"`" in leading position
//     (followed by ". The `.v1` suffix is the version slot"). The
//     memory_backfill schema bullet at line 649 instead opens with
//     "literal string `\"covenant.memory.backfill.v1\"`. Same
//     versioning semantics as ..." — different leading literal, so
//     first-match capture cannot drift.
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

const testFnName = "settlement_backfill_json_pins_top_level_schema";

const targets = [
  {
    field: "schema",
    selector: 'value["schema"].is_string(),',
    docsRegex:
      /- `schema`: literal string `"covenant\.settlement\.backfill\.v1"`\. The `\.v1` suffix is the version slot; a future `\.v2` would be a separate envelope, not a field rename inside this one\. Consumers must route on the full literal — matching on the prefix `"covenant\.settlement\.backfill\."` will swallow incompatible future versions\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never an integer or object\./,
    docsLabel: "settlement_backfill.schema type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never an integer or object.",
  },
  {
    field: "row_count",
    selector: 'value["row_count"].is_u64(),',
    docsRegex:
      /- `row_count` \(u64\): count of legacy settlement-receipt rows the backfill operated on \(mutation path\) or \*would\* operate on \(dry-run path\)\. May legitimately be `0` when no legacy rows match — the verb does not error on an empty backfill\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string-of-integer\./,
    docsLabel: "settlement_backfill.row_count type-level pin citation",
    docsTemplate:
      "Pinned as u64 by `main.rs:N-M` — never a string-of-integer.",
  },
  {
    field: "rollback_path",
    selector:
      'value["rollback_path"].is_string() || value["rollback_path"].is_null(),',
    docsRegex:
      /- `rollback_path` \(string or null\): filesystem path to the rollback-evidence file written by a mutation pass; `null` in dry-run mode\. The CLI's inline emission at `main\.rs:\d+` passes `rollback_path\.as_deref\(\)` through `Option<&str>`, and the unsuffixed CLI at `main\.rs:\d+-\d+` maps `None` to the literal `\(none\)` — JSON consumers must use `null` \(not `""` or `"\(none\)"`\) as the unset discriminator\. When non-null, the path is meaningful only on the daemon's local filesystem; remote consumers must not assume the file is reachable\. Pinned as string-or-null by `main\.rs:(\d+)-(\d+)` — never the literal `"\(none\)"`\./,
    docsLabel: "settlement_backfill.rollback_path type-level pin citation",
    docsTemplate:
      "Pinned as string-or-null by `main.rs:N-M` — never the literal `\"(none)\"`.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the settlement_backfill pins-test still exists and is not renamed or duplicated`,
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
  console.error("validate-settlement-backfill-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
