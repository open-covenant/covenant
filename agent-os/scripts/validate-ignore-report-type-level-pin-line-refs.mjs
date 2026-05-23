#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ignore_report envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two inner assertion ranges
// inside ignore_report_json_pins_top_level_schema:
//
//   - line 579 cites `ignored` (boolean) type pin.
//   - line 580 cites `matched_pattern` (string or null) type pin.
//
// Both cites are currently correct under the assert!-opener-to-closer
// 4-line convention. This validator lands proactively — sibling
// envelope pins-tests (a2a_status, tool_result, intent_result) saw
// ~222-line drift events before their type-level pin cites were
// guarded; ignore_report is added to the guarded set before its first
// drift event lands.
//
// The validator scopes each lookup to the brace-balanced
// `ignore_report_json_pins_top_level_schema` fn body so the same
// selectors inside the ignore_report_json_renders_stable_shape
// assert_eq! value checks cannot contaminate the result.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "ignore_report_json_pins_top_level_schema";

const targets = [
  {
    field: "ignored",
    selector: 'value["ignored"].is_boolean(),',
    docsRegex:
      /- `ignored` \(boolean\): `true` when at least one loaded rule matched the supplied text; `false` otherwise\. Pinned as a JSON boolean by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never `0`\/`1` or a string-truthy value\./,
    docsLabel: "ignore_report.ignored type-level pin citation",
    docsTemplate:
      "Pinned as a JSON boolean by the schema test (`main.rs:N-M`) — never `0`/`1` or a string-truthy value.",
  },
  {
    field: "matched_pattern",
    selector:
      'value["matched_pattern"].is_string() || value["matched_pattern"].is_null(),',
    docsRegex:
      /- `matched_pattern` \(string or null\): the matched rule pattern when `ignored` is `true`; \*\*always `null`\*\* when `ignored` is `false`\. Pinned as string-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never an empty string for the unmatched case\./,
    docsLabel: "ignore_report.matched_pattern type-level pin citation",
    docsTemplate:
      "Pinned as string-or-null by the schema test (`main.rs:N-M`) — never an empty string for the unmatched case.",
  },
  {
    field: "rules_loaded",
    selector: 'value["rules_loaded"].is_u64(),',
    docsRegex:
      /- `rules_loaded` \(u64\): count of ignore rules the daemon evaluated\. May legitimately be `0` when no rules are configured, in which case `ignored` is always `false` and `matched_pattern` is always `null`\. Pinned as u64 by `main\.rs:(\d+)-(\d+)` — never a string-of-integer\./,
    docsLabel: "ignore_report.rules_loaded type-level pin citation",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the ignore_report pins-test still exists and is not renamed or duplicated`,
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
  console.error("validate-ignore-report-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ignore-report-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
