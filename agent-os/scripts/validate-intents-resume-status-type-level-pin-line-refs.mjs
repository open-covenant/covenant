#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume_ok status type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 615 cites the main.rs line range
// for the type-level pin on `value["status"].is_string()` inside
// `intents_resume_ok_json_pins_top_level_schema`. The assertion is the
// 4-line assert!-opener-to-closer form:
//
//   assert!(
//       value["status"].is_string(),
//       "status must be a string: {value}",
//   );
//
// so the docs cite is a `main.rs:N-M` range, matching the multi-line
// convention shared with the sibling
// validate-intent-result-status-type-level-pin-line-refs.mjs.
//
// Selector form: `value["status"].is_string(),` recurs across many
// pins-tests (intent_result, intents_resume_error, bootstrap_result,
// audit_verify, ...). Brace-scoping to intents_resume_ok_json_pins_top_
// level_schema isolates this match.
//
// docsRegex anchoring: line 236 carries a sibling `status` bullet for
// the intent_result envelope with the identical canonical "Pinned as a
// string by `main.rs:N-M` — never an object or array. Specific value
// enumeration lives with..." trailer. The two differ only at the head:
// intent_result uses a colon separator and "the outcome status (e.g.,"
// while intents_resume_ok uses an em-dash separator and "the daemon-
// returned outcome status (typically". The regex anchors on the
// intents_resume_ok head so it reads only its own range.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "intents_resume_ok_json_pins_top_level_schema";
const selector = 'value["status"].is_string(),';

const docsRegex =
  /- `status` \(string\) — the daemon-returned outcome status \(typically `"ok"`\)\. Pinned as a string by `main\.rs:(\d+)-(\d+)` — never an object or array\. Specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface\./;
const docsLabel = "intents_resume_ok.status type-level pin citation";
const docsTemplate =
  "- `status` (string) — the daemon-returned outcome status (typically `\"ok\"`). Pinned as a string by `main.rs:N-M` — never an object or array. Specific value enumeration lives with the daemon's intent dispatcher rather than this docs surface.";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the intents_resume_ok pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the status type-level assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-to-closing convention requires the assert!( opener on the line directly above the selector`,
          );
        } else {
          let closer = null;
          for (let index = selectorLine; index < testEnd; index += 1) {
            if (lines[index].trim() === ");") {
              closer = index + 1;
              break;
            }
          }
          if (closer === null) {
            fail(
              `${sourcePath}: could not find the closing \`);\` after the status selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
            );
          } else {
            startLine = assertOpenerLine;
            endLine = closer;
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the status type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the status type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-status-type-level-pin-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-status-type-level-pin-line-refs: ok (status main.rs:${startLine}-${endLine})`,
);
