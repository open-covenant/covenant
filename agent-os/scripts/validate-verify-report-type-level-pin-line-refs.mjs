#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// verify_report envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites inner assertion ranges inside
// verify_report_json_pins_top_level_schema:
//
//   - line 116 cites `window` (u64) type pin.
//   - line 117 cites `checks` (array) type pin.
//   - line 119 cites `orphans_total` (u64) type pin.
//
// The existing validate-verify-report-line-refs.mjs covers the helper
// fn, pins test declaration line, and the test-body range, but not
// the inner type-level selector ranges. Each cite would otherwise go
// stale silently whenever main.rs grew inside the pins test fn body.
//
// The validator scopes each lookup to the brace-balanced
// `verify_report_json_pins_top_level_schema` fn body. Each target's
// selector is matched as an exact trim against the value[...].is_u64(),
// line; the brace-scoping plus exact-match isolates verify_report's
// pins even if a sibling envelope's pins test later carries a
// same-named field. Each range starts at the `assert!(` opener line
// directly above the selector (assert!-opener-to-closer 4-line
// convention, mirroring validate-peer-list-type-level-pin-line-refs.mjs)
// and ends at the next closing `);` line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "verify_report_json_pins_top_level_schema";

const targets = [
  {
    field: "window",
    selectorFirstLine: 'value["window"].is_u64(),',
    docsRegex:
      /- `window` \(u64\): the audit-window record count echoed back from the `--window` argument\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string\./,
    docsLabel: "verify_report.window type-level pin citation",
    docsTemplate: "Pinned as u64 by `main.rs:N-M` — never a string.",
  },
  {
    field: "checks",
    selectorFirstLine: 'value["checks"].is_array(),',
    docsRegex:
      /- `checks` \(array of `VerifyCheck`\): per-check results, see below\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "verify_report.checks type-level pin citation",
    docsTemplate: "Pinned as an array by `main.rs:N-M` — never null or a string.",
  },
  {
    field: "orphans_total",
    selectorFirstLine: 'value["orphans_total"].is_u64(),',
    docsRegex:
      /- `orphans_total` \(u64\): total number of unmatched rows the checks discovered\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string-of-integer\./,
    docsLabel: "verify_report.orphans_total type-level pin citation",
    docsTemplate:
      "Pinned as u64 by `main.rs:N-M` — never a string-of-integer.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the verify_report pins-test still exists and is not renamed or duplicated`,
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
          if (lines[index].trim() === target.selectorFirstLine) {
            selectorMatches.push(index + 1);
          }
        }
        if (selectorMatches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 occurrence of \`${target.selectorFirstLine}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion's first line is present exactly once in this test`,
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
            `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selectorFirstLine}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector for the ${target.field} type pin`,
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
  console.error("validate-verify-report-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-verify-report-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
