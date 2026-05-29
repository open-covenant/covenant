#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// audit_verify report.root_hash_hex length pin line-ref drift guard.
// docs/ipc-and-http-gateway.md line 391 cites a main.rs range for the
// audit_verify report.root_hash_hex length pin.
//
// The cite previously used a single-line shape (just the `.len(),`
// fragment line); this validator now uses the assert_eq!-opener-to-
// closer range convention that the memory_compaction_plan validator
// established for assert_eq! pins. The range convention catches a
// wider set of drift modes: the multi-line selector grows or shrinks
// (e.g., the .as_str() / .unwrap_or_default() chain gets refactored)
// while the .len(), fragment line stays at a different relative
// position; a single-line cite would silently stay correct, while the
// 7-line range cite fails loudly.
//
// Unlike the other type-pin validators in this family, this one
// scopes to `audit_verify_json_renders_stable_shape` (a stable-shape
// test), not `_pins_top_level_schema`. The length pin lives in the
// former. The selector value["report"]["root_hash_hex"] (no suffix,
// no chained methods) is currently unique to this assert_eq! in the
// entire file, so brace-balanced fn body scoping plus exact-trim-
// match isolates it cleanly.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const testFnName = "audit_verify_json_renders_stable_shape";
const selector = 'value["report"]["root_hash_hex"]';

const docsRegex =
  /- `root_hash_hex` \(string\) — the final root hash as lowercase hex, 64 characters \(SHA-256\)\. Pinned at the length level by the stable-shape test at `main\.rs:(\d+)-(\d+)`\./;
const docsLabel = "audit_verify report.root_hash_hex length pin citation";
const docsTemplate =
  "Pinned at the length level by the stable-shape test at `main.rs:N-M`.";

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
      `${sourcePath}: expected exactly 1 "fn ${testFnName}" inside tests module but found ${testMatches.length}; remediation: confirm the audit_verify renders_stable_shape test still exists and is not renamed or duplicated`,
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
          `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` inside ${testFnName} (lines ${testStart}-${testEnd}) but found ${selectorMatches.length}; remediation: confirm the root_hash_hex length pin assertion is present exactly once in this test`,
        );
      } else {
        const selectorLine = selectorMatches[0];
        const assertOpenerLine = selectorLine - 1;
        if (
          assertOpenerLine < 1 ||
          lines[assertOpenerLine - 1].trim() !== "assert_eq!("
        ) {
          fail(
            `${sourcePath}:${assertOpenerLine}: expected line above \`${selector}\` to contain exactly \`assert_eq!(\`, but found \`${lines[assertOpenerLine - 1]}\`; remediation: the assert_eq!-to-closing convention requires the assert_eq!( opener on the line directly above the selector`,
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
              `${sourcePath}: could not find the closing \`);\` after the root_hash_hex selector at line ${selectorLine}; remediation: confirm the surrounding assert_eq! macro is closed on its own line`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the root_hash_hex length pin line range as the modern :N-M form (not the single-line :N form)`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the root_hash_hex length pin assertion spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-audit-verify-root-hash-hex-length-pin-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-audit-verify-root-hash-hex-length-pin-line-refs: ok (root_hash_hex main.rs:${startLine}-${endLine})`,
);
