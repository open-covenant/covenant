#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_result envelope type-level pin line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two inner assertion ranges
// inside tool_result_json_pins_top_level_schema:
//
//   - line 159 cites `content` (array) type pin.
//   - line 160 cites `is_error` (boolean) type pin.
//
// The is_error cite landed first as a single-target validator (after
// recovering from a ~222-line drift event that pointed above the test
// fn opener); the content cite was added later when the validator was
// converted to the multi-target shape (mirroring
// validate-bootstrap-result-type-level-pin-line-refs.mjs).
//
// The validator scopes each lookup to the brace-balanced
// `tool_result_json_pins_top_level_schema` fn body so the same
// `value["content"].is_array(),` selector inside an unrelated future
// envelope's pins test cannot contaminate the result. The content
// docsRegex anchors on the tool_result-specific prose ("the tool's
// output blocks. Each element is a tagged-enum object whose `type`
// discriminator selects the variant") so it does not collide with the
// sibling A2A content bullet at line 468 which uses "the same
// tagged-enum `Content` shape... already documented".

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "tool_result_json_pins_top_level_schema";

const targets = [
  {
    field: "content",
    selector: 'value["content"].is_array(),',
    docsRegex:
      /- `content` \(array of `Content`\): the tool's output blocks\. Each element is a tagged-enum object whose `type` discriminator selects the variant — `\{type: "text", text: <string>\}` for textual output or `\{type: "json", value: <JSON>\}` for structured output\. The variants are defined at `agent-os\/crates\/covenant-mcp\/src\/lib\.rs:\d+` with `#\[serde\(tag = "type", rename_all = "camelCase"\)\]`; v0 ships text and json variants only\. The array is empty when the tool produced no output blocks; the unsuffixed CLI prints each block sequentially at `main\.rs:\d+-\d+`\. Pinned as an array by `main\.rs:(\d+)-(\d+)` — never null or a string\./,
    docsLabel: "tool_result.content type-level pin citation",
    docsTemplate:
      "Pinned as an array by `main.rs:N-M` — never null or a string.",
  },
  {
    field: "is_error",
    selector: 'value["is_error"].is_boolean(),',
    docsRegex:
      /- `is_error` \(boolean\): `true` when the tool itself raised; pinned as a JSON boolean by the schema test \(`main\.rs:(\d+)-(\d+)`\) — never `0`\/`1` or a string\./,
    docsLabel: "tool_result.is_error type-level pin citation",
    docsTemplate:
      "pinned as a JSON boolean by the schema test (`main.rs:N-M`) — never `0`/`1` or a string.",
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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the tool_result pins-test still exists and is not renamed or duplicated`,
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
  console.error("validate-tool-result-type-level-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-result-type-level-pin-line-refs: ok (${targets.map((t) => `${t.field} main.rs:${t.startLine}-${t.endLine}`).join(", ")})`,
);
