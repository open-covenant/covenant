#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement.backfill CLI 3-line print range line-ref drift
// guard. docs/ipc-and-http-gateway.md line 643 cites a six-line
// main.rs range for the CLI block that prints the unsuffixed
// settlement.backfill success path's three lines: `row_count: <N>`,
// `dry_run: <bool>`, and `rollback_path: <path>|(none)`. The
// `rollback_path` print is a multi-line println! because the value
// expression `rollback_path.as_deref().unwrap_or("(none)")` does
// not fit inline, so the cited range covers the two single-line
// prints (rows 1-2) plus the four-line rollback_path println!
// (rows 3-6).
//
// Discriminator anchor: the line
// `rollback_path.as_deref().unwrap_or("(none)")` is unique to
// this block (verified by grep -nF). The memory.backfill sibling
// print block at main.rs:2393-2395 uses the single-line
// `println!("savepoint_name: {savepoint_name}");` instead, so the
// `rollback_path` value-expression anchor cleanly distinguishes
// the two sibling blocks.
//
// Structural verification: from the anchor line N, the validator
// asserts the six lines N-4..N+1 match the exact expected shape.
// A refactor that, e.g., inlines the value expression into the
// format string would shift only the closer line and surface the
// structural change at validate time.
//
// docsRegex anchoring: docs line 643 contains four main.rs cites
// (CLI wiring 4034-4088 covered by sibling validator, helper fn
// 4558, pins-test 5550, plus this CLI print range). The regex
// anchors on the unique trailing phrase including the
// rollback_path-specific print form so first-match capture
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
  'rollback_path.as_deref().unwrap_or("(none)")';
// Layout from the anchor line (offset = lineNumber - anchorLine):
//   -4  println!("row_count: {row_count}");
//   -3  println!("dry_run: {dry_run}");
//   -2  println!(
//   -1  "rollback_path: {}",
//    0  rollback_path.as_deref().unwrap_or("(none)")   <-- anchor
//   +1  );
const expectedShape = [
  { offset: -4, content: 'println!("row_count: {row_count}");' },
  { offset: -3, content: 'println!("dry_run: {dry_run}");' },
  { offset: -2, content: "println!(" },
  { offset: -1, content: '"rollback_path: {}",' },
  { offset: 1, content: ");" },
];

const docsRegex =
  /prints `row_count: <N>`, `dry_run: <bool>`, and `rollback_path: <path>\|\(none\)` on three separate lines at `main\.rs:(\d+)-(\d+)`/;
const docsLabel =
  "settlement.backfill CLI 3-line print range citation";
const docsTemplate =
  "prints `row_count: <N>`, `dry_run: <bool>`, and `rollback_path: <path>|(none)` on three separate lines at `main.rs:N-M`";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${anchorSelector}\` but found ${candidates.length}; remediation: confirm the settlement.backfill CLI rollback_path expression is present exactly once on the success path`,
    );
  } else {
    const anchorLine = candidates[0];
    let shapeOk = true;
    for (const expected of expectedShape) {
      const targetLine = anchorLine + expected.offset;
      const actual = lines[targetLine - 1];
      if (actual === undefined || actual.trim() !== expected.content) {
        fail(
          `${sourcePath}:${targetLine}: expected \`${expected.content}\` (offset ${expected.offset} from rollback_path anchor) but found \`${(actual ?? "").trim()}\`; remediation: the six-line print block convention requires this layout — restore the structural shape or update this validator if the convention intentionally changed`,
        );
        shapeOk = false;
      }
    }
    if (shapeOk) {
      startLine = anchorLine - 4;
      endLine = anchorLine + 1;
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the settlement.backfill CLI 3-line print range`,
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
  console.error(
    "validate-settlement-backfill-cli-print-range-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-cli-print-range-line-refs: ok (CLI print block main.rs:${startLine}-${endLine})`,
);
