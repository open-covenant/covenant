#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_status envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites five inner assertion ranges
// inside a2a_status_json_pins_top_level_schema:
//
//   - line 437 cites `limit` (u64) type pin.
//   - line 438 cites `min_lease_age_ms` (u64 or null) type pin.
//   - line 439 cites `deadline_within_ms` (u64 or null) type pin.
//   - line 440 cites `state_filter` (string or null) type pin.
//   - line 442 cites `results` (array) type pin at :7421-7424.
//
// The existing validate-a2a-status-line-refs.mjs covers the helper fn,
// renders test, and pins test declaration lines, but not the inner
// type-level selector ranges. All four cites were stale by ~222 lines
// at one point or another — the state_filter cite was the first one
// to be repaired; this slice fixes the remaining three (limit,
// min_lease_age_ms, deadline_within_ms).
//
// The validator scopes each lookup to the brace-balanced
// `a2a_status_json_pins_top_level_schema` fn body, then derives the
// range as the line containing `assert!(` through the next closing
// `);` line — the 4-line assert!-opener-to-closer convention shared
// by all four cites.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "a2a_status_json_pins_top_level_schema";

const targets = [
  {
    field: "limit",
    selector: 'value["limit"].is_u64(),',
    docsRegex:
      /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, per `main\.rs:\d+`\)\. Pinned as u64 by the schema test \(`main\.rs:(\d+)-(\d+)`\)\./,
    docsLabel: "a2a_status.limit type-level pin citation",
    docsTemplate:
      "Pinned as u64 by the schema test (`main.rs:N-M`).",
  },
  {
    field: "min_lease_age_ms",
    selector:
      'value["min_lease_age_ms"].is_u64() || value["min_lease_age_ms"].is_null(),',
    docsRegex:
      /- `min_lease_age_ms` \(u64 or null\): the threshold echoed from `--min-lease-age-ms`, or `null` when the flag was omitted\. Always emitted \(as `null` when inactive\) — never omitted from the envelope\. Pinned as u64-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\)\./,
    docsLabel: "a2a_status.min_lease_age_ms type-level pin citation",
    docsTemplate:
      "Pinned as u64-or-null by the schema test (`main.rs:N-M`).",
  },
  {
    field: "deadline_within_ms",
    selector:
      'value["deadline_within_ms"].is_u64() || value["deadline_within_ms"].is_null(),',
    docsRegex:
      /- `deadline_within_ms` \(u64 or null\): the threshold echoed from `--deadline-within-ms`, or `null` when the flag was omitted\. Same always-emitted-as-null contract as `min_lease_age_ms`\. Pinned as u64-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\)\./,
    docsLabel: "a2a_status.deadline_within_ms type-level pin citation",
    docsTemplate:
      "Pinned as u64-or-null by the schema test (`main.rs:N-M`).",
  },
  {
    field: "state_filter",
    selector:
      'value["state_filter"].is_string() || value["state_filter"].is_null(),',
    docsRegex:
      /- `state_filter` \(string or null\): the `A2ATaskQueueState` slug echoed from `--state` — exactly `"queued"` or `"in_flight"` \(snake_case, per `A2ATaskQueueState`'s `#\[serde\(rename_all = "snake_case"\)\]` at `covenant-a2a\/src\/lib\.rs:\d+-\d+`\), or `null` when the flag was omitted\. Pinned as string-or-null by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never an integer or array\./,
    docsLabel: "a2a_status.state_filter type-level pin citation",
    docsTemplate:
      "Pinned as string-or-null by the schema test (`main.rs:N-M`) — never an integer or array.",
  },
  {
    field: "results",
    selector: 'value["results"].is_array(),',
    docsRegex:
      /- `results` \(array of `A2ATaskResult`\): pending results not yet acknowledged\. The array may be empty; the unsuffixed CLI prints `\(a2a queue empty\)` at `main\.rs:\d+` when both `tasks` and `results` are empty\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "a2a_status.results type-level pin citation",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the a2a_status pins-test still exists and is not renamed or duplicated`,
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
  console.error("validate-a2a-status-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
