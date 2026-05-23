#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume envelopes type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites eight inner assertion ranges that
// pin types in the intents_resume envelopes:
//
//   - line 619 cites the error envelope's `intent_id` (string or null)
//     at main.rs:5224-5227 — inside
//     intents_resume_error_json_pins_top_level_schema.
//   - line 620 cites the error envelope's `error` (object) at
//     main.rs:5228-5231 — inside
//     intents_resume_error_json_pins_top_level_schema.
//   - line 622 cites the error envelope's inner `error.message`
//     (string) at main.rs:5269-5272 — inside
//     intents_resume_error_json_pins_error_object_schema (a different
//     test fn than the top-level pins-tests).
//   - line 615 cites the ok envelope's `settlement` (object or null)
//     at main.rs:5389-5392 — inside
//     intents_resume_ok_json_pins_top_level_schema.
//   - line 612 cites the ok envelope's `status` (string) at
//     main.rs:5380-5383 — inside
//     intents_resume_ok_json_pins_top_level_schema.
//   - line 611 cites the ok envelope's `intent_id` (string) at
//     main.rs:5377-5379 — inside
//     intents_resume_ok_json_pins_top_level_schema. This cite uses the
//     3-line selector-to-closer convention (no assert!( opener line);
//     every other target uses the 4-line assert!-opener-to-closer
//     convention. Per-target `convention` field selects between the
//     two; default is "assert-opener" to keep prior targets correct.
//   - line 606 cites the ok envelope's `ok` (boolean) at the first
//     range, inside intents_resume_ok_json_pins_top_level_schema.
//   - line 606 cites the error envelope's `ok` (boolean) at the second
//     range, inside intents_resume_error_json_pins_top_level_schema.
//
// All docs cites use the assert!-to-closing convention: the cite spans
// from the line containing `assert!(` through the closing `);` line.
// This convention is chosen to match the existing docs cites without
// requiring a docs prose change.
//
// The two ok-pin cites share a single docs sentence with two cites in
// it. Each ok target's docs regex anchors on the cite's unique context
// ("tests at " for the ok-branch cite, " and " for the error-branch
// cite) so each target reads only its own range from the sentence.
//
// The validator scopes each lookup to the brace-balanced test fn body
// so a same-named selector inside a different envelope's pins test
// cannot contaminate the result. Each target derives its range start
// as the `assert!(` line one line above the matched selector line.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const targets = [
  {
    field: "intent_id",
    testFnName: "intents_resume_error_json_pins_top_level_schema",
    selector: 'value["intent_id"].is_string() || value["intent_id"].is_null(),',
    docsRegex:
      /- `intent_id` \(string or null\) — string-uuid when the intent_id was already resolved \(e\.g\., the daemon round-trip started but returned an error, per `main\.rs:\d+-\d+`\); \*\*null\*\* when the intent_id could not be resolved before the daemon round-trip \(e\.g\., `missing_intent_id` and `conflicting_flags` paths at `main\.rs:\d+-\d+`\)\. Pinned as string-or-null by `main\.rs:(\d+)-(\d+)`\./,
    docsLabel: "intents_resume_error.intent_id type-level pin citation",
    docsTemplate: "Pinned as string-or-null by `main.rs:N-M`.",
  },
  {
    field: "settlement",
    testFnName: "intents_resume_ok_json_pins_top_level_schema",
    selector:
      'value["settlement"].is_object() || value["settlement"].is_null(),',
    docsRegex:
      /- `settlement` \(object or null\) — an optional `SettlementReceipt` \(defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:\d+` and documented in the `receipt_list` block above\) carrying the on-chain or local settlement evidence when the resumed intent consumed credits\. `null` when the resume did not settle\. Pinned as object-or-null by `main\.rs:(\d+)-(\d+)` — never an integer or array\./,
    docsLabel: "intents_resume_ok.settlement type-level pin citation",
    docsTemplate:
      "Pinned as object-or-null by `main.rs:N-M` — never an integer or array.",
  },
  {
    field: "ok_in_ok_branch",
    testFnName: "intents_resume_ok_json_pins_top_level_schema",
    selector: 'value["ok"].is_boolean(),',
    docsRegex:
      /Pinned as a JSON boolean by the schema tests at `main\.rs:(\d+)-(\d+)` and `main\.rs:\d+-\d+` — never `0`\/`1` or a string-truthy value\./,
    docsLabel: "intents_resume_ok.ok type-level pin citation",
    docsTemplate:
      "Pinned as a JSON boolean by the schema tests at `main.rs:N-M` and `main.rs:N-M` — never `0`/`1` or a string-truthy value.",
  },
  {
    field: "ok_in_error_branch",
    testFnName: "intents_resume_error_json_pins_top_level_schema",
    selector: 'value["ok"].is_boolean(),',
    docsRegex:
      /Pinned as a JSON boolean by the schema tests at `main\.rs:\d+-\d+` and `main\.rs:(\d+)-(\d+)` — never `0`\/`1` or a string-truthy value\./,
    docsLabel: "intents_resume_error.ok type-level pin citation",
    docsTemplate:
      "Pinned as a JSON boolean by the schema tests at `main.rs:N-M` and `main.rs:N-M` — never `0`/`1` or a string-truthy value.",
  },
  {
    field: "status",
    testFnName: "intents_resume_ok_json_pins_top_level_schema",
    selector: 'value["status"].is_string(),',
    docsRegex:
      /- `status` \(string\) — the daemon-returned outcome status \(typically `"ok"`\)\. The string shape is pinned at `main\.rs:(\d+)-(\d+)`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface\./,
    docsLabel: "intents_resume_ok.status type-level pin citation",
    docsTemplate:
      "The string shape is pinned at `main.rs:N-M`; specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.",
  },
  {
    field: "error",
    testFnName: "intents_resume_error_json_pins_top_level_schema",
    selector: 'value["error"].is_object(),',
    docsRegex:
      /- `error` \(object\): a structured `\{code, message\}` pair, never a string blob\. Pinned as a JSON object by `main\.rs:(\d+)-(\d+)`\. The inner `error` object has exactly two keys per the test EXPECTED_KEYS at `main\.rs:\d+`:/,
    docsLabel: "intents_resume_error.error type-level pin citation",
    docsTemplate:
      "Pinned as a JSON object by `main.rs:N-M`. The inner `error` object has exactly two keys per the test EXPECTED_KEYS at `main.rs:N`:",
  },
  {
    field: "error_message",
    testFnName: "intents_resume_error_json_pins_error_object_schema",
    selector: 'value["error"]["message"].is_string(),',
    docsRegex:
      /- `message` \(string\) — the human-readable diagnostic the daemon or CLI produced\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never a structured object\./,
    docsLabel: "intents_resume_error.error.message type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never a structured object.",
  },
  {
    field: "intent_id_in_ok_branch",
    testFnName: "intents_resume_ok_json_pins_top_level_schema",
    selector: 'value["intent_id"].is_string(),',
    docsRegex:
      /- `intent_id` \(string\) — the resumed intent's UUID in canonical hyphenated form\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never a byte array\./,
    docsLabel: "intents_resume_ok.intent_id type-level pin citation",
    docsTemplate:
      "Pinned as a string by `main.rs:N-M` — never a byte array.",
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
  for (const target of targets) {
    const testOpenerRegex = new RegExp(`^\\s+fn\\s+${target.testFnName}\\s*\\(`);
    const testMatches = [];
    for (let index = 0; index < lines.length; index += 1) {
      if (testOpenerRegex.test(lines[index])) {
        testMatches.push(index + 1);
      }
    }
    if (testMatches.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "fn ${target.testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the ${target.field} pins-test still exists and is not renamed or duplicated`,
      );
      continue;
    }
    const testStart = testMatches[0];
    const testEnd = scanBraceBalance(lines, testStart);
    if (testEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "fn ${target.testFnName}" starting at line ${testStart}; remediation: confirm the test fn body is brace-balanced`,
      );
      continue;
    }
    const selectorMatches = [];
    for (let index = testStart; index < testEnd; index += 1) {
      if (lines[index].trim() === target.selector) {
        selectorMatches.push(index + 1);
      }
    }
    if (selectorMatches.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 occurrence of \`${target.selector}\` inside ${target.testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the ${target.field} type-level assertion is present exactly once in this test`,
      );
      continue;
    }
    const selectorLine = selectorMatches[0];
    const convention = target.convention ?? "assert-opener";
    if (convention === "assert-opener") {
      const assertOpenerLine = selectorLine - 1;
      if (assertOpenerLine < 1 || lines[assertOpenerLine - 1].trim() !== "assert!(") {
        fail(
          `${sourcePath}:${assertOpenerLine}: expected line above \`${target.selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-to-closing range convention requires the assert!( opener on the line directly above the selector`,
        );
        continue;
      }
      target.startLine = assertOpenerLine;
    } else {
      target.startLine = selectorLine;
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
    target.endLine = endLine;
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
  console.error("validate-intents-resume-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
