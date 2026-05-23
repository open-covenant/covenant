#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_list envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 141 cites a single main.rs range
// for the tool_list.tools (array) type pin inside the
// tool_list_json_pins_top_level_schema fn body.
//
// Before this validator landed, the docs prose described `tools` as
// "(array of ToolSpec)" with no main.rs cite. The assertion at
// main.rs:6972-6975 (assert!(value["tools"].is_array(), ...);) was
// only enforced at test runtime. The validator binds the docs prose
// to the source-of-truth so a regression that turned the field into
// a string blob or name-keyed map surfaces at the docs-validator
// level (not just at test runtime).
//
// The validator scopes its lookup to the brace-balanced
// `tool_list_json_pins_top_level_schema` fn body. The
// value["tools"].is_array(), selector is currently unique inside
// main.rs (this is the only tool_list envelope assertion using that
// exact form), but the brace-scoping plus exact-trim-match also
// isolates the tool_list one if a sibling envelope later adds the
// same selector. The range is derived as assert!-opener-to-closer
// (4-line convention) — the cite spans the `assert!(` opener
// directly above the selector through the closing `);` on its own
// line, mirroring validate-capability-revoke-type-level-pin-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "tool_list_json_pins_top_level_schema";
const selector = 'value["tools"].is_array(),';

const docsRegex =
  /- `tools` \(array of `ToolSpec`\): the registered tools the daemon advertises via `tools\/list`\. The array is empty when no tools are registered; the unsuffixed CLI prints `\(no tools registered\)` for that case at `main\.rs:3123`\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string blob\./;
const docsLabel = "tool_list.tools type-level pin citation";
const docsTemplate =
  "Pinned as an array by `main.rs:N-M` — never null or a string blob.";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the tool_list pins-test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the tools type-level assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert!-opener-to-closer convention requires the assert!( opener on the line directly above the selector`,
          );
        } else {
          startLine = assertOpenerLine;
          for (let index = selectorLine; index < testEnd; index += 1) {
            if (lines[index].trim() === ");") {
              endLine = index + 1;
              break;
            }
          }
          if (endLine === null) {
            fail(
              `${sourcePath}: could not find the closing \`);\` after the tools selector at line ${selectorLine}; remediation: confirm the surrounding assert! macro is closed on its own line`,
            );
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the tools type-level pin line range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the tools type-level assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-tool-list-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-list-type-level-pin-line-refs: ok (tool_list.tools main.rs:${startLine}-${endLine})`,
);
