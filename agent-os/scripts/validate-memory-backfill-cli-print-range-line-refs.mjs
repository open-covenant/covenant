#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill CLI 3-line print range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 660 cites a three-line
// main.rs range for the CLI block that prints the unsuffixed
// memory.backfill success path's three lines: `row_count: <N>`,
// `dry_run: <bool>`, and `savepoint_name: <name>`. Unlike the
// settlement.backfill sibling block at main.rs:4077-4082 (which
// spans six source lines because the rollback_path value is too
// long to inline), the savepoint_name print fits on a single line
// so the cited range is just three lines.
//
// Discriminator anchor: the line
// `println!("savepoint_name: {savepoint_name}");` is unique to
// this block (verified by grep -nF). The settlement.backfill
// sibling uses a multi-line rollback_path println instead, so the
// `savepoint_name` literal cleanly distinguishes the two sibling
// blocks.
//
// Structural verification: from the anchor line N, the validator
// asserts the two lines N-2 and N-1 trim to the row_count and
// dry_run prints respectively. A refactor that, e.g., reorders
// the print lines or inserts an intermediate computation would
// surface the structural change at validate time.
//
// docsRegex anchoring: docs line 660 contains four main.rs cites
// (CLI wiring 2350-2401 covered by sibling validator, helper fn
// 4571, pins-test 5613, plus this CLI print range). The regex
// anchors on the unique trailing phrase including the
// savepoint_name-specific print form so first-match capture
// targets only this cite, per the IPC docs pin collision anchor
// feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const anchorSelector =
  'println!("savepoint_name: {savepoint_name}");';
// Layout from the anchor line (offset = lineNumber - anchorLine):
//   -2  println!("row_count: {row_count}");
//   -1  println!("dry_run: {dry_run}");
//    0  println!("savepoint_name: {savepoint_name}");   <-- anchor
const expectedPreceding = [
  { offset: -2, content: 'println!("row_count: {row_count}");' },
  { offset: -1, content: 'println!("dry_run: {dry_run}");' },
];

const docsRegex =
  /prints `row_count: <N>`, `dry_run: <bool>`, and `savepoint_name: <name>` on three separate lines at `main\.rs:(\d+)-(\d+)`/;
const docsLabel =
  "memory.backfill CLI 3-line print range citation";
const docsTemplate =
  "prints `row_count: <N>`, `dry_run: <bool>`, and `savepoint_name: <name>` on three separate lines at `main.rs:N-M`";

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

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === anchorSelector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${anchorSelector}\` but found ${candidates.length}; remediation: confirm the memory.backfill CLI savepoint_name print is present exactly once on the success path`,
    );
  } else {
    const anchorLine = candidates[0];
    let shapeOk = true;
    for (const expected of expectedPreceding) {
      const targetLine = anchorLine + expected.offset;
      const actual = lines[targetLine - 1];
      if (actual === undefined || actual.trim() !== expected.content) {
        fail(
          `${sourcePath}:${targetLine}: expected \`${expected.content}\` (offset ${expected.offset} from savepoint_name anchor) but found \`${(actual ?? "").trim()}\`; remediation: the three-line print block convention requires this layout — restore the structural shape or update this validator if the convention intentionally changed`,
        );
        shapeOk = false;
      }
    }
    if (shapeOk) {
      startLine = anchorLine - 2;
      endLine = anchorLine;
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the memory.backfill CLI 3-line print range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the print block lives at :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-memory-backfill-cli-print-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-cli-print-range-line-refs: ok (CLI print block main.rs:${startLine}-${endLine})`,
);
