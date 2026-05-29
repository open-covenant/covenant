#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement.backfill rollback_path inline-cite double-citation line-ref
// drift guard. docs/ipc-and-http-gateway.md line 636 (the rollback_path
// bullet) carries two inline main.rs cites in one sentence:
//
//   * `main.rs:N1` — the inline emission `rollback_path.as_deref(),`
//     passed as the second positional arg to settlement_backfill_json's
//     emitter inside the `if as_json {` branch of the settlement
//     backfill-receipts CLI handler. Single-line cite.
//   * `main.rs:N2-M2` — the unsuffixed CLI's three-line `println!`
//     block emitting `rollback_path: {}` and mapping the `None` arm to
//     the literal `(none)`. The docs cite excludes the println! opener
//     (one line above the format string) by docs-author choice; the
//     cited range spans format-string -> argument -> closer only.
//
// Both cites are correct today but unbound by any existing validator:
// validate-settlement-backfill-type-level-pin-line-refs.mjs's
// rollback_path docsRegex matches the same bullet but uses non-capturing
// `\d+` / `\d+-\d+` placeholders for these two cites and asserts only
// the trailing Pinned-as type-pin cite (5578-5581). The two validators
// coexist with disjoint capture targets.
//
// Anchors:
//   * Anchor 1: the trimmed unique line `rollback_path.as_deref(),`
//     (with trailing comma) appears exactly once in main.rs (verified
//     by grep -nF). Single-line cite at the anchor.
//   * Anchor 2: the trimmed unique line
//     `rollback_path.as_deref().unwrap_or("(none)")` appears exactly
//     once in main.rs. Walk back 1 line for the format-string
//     `"rollback_path: {}",` (range start) and forward 1 line for the
//     closing `);` (range end). Both walk distances are verified by
//     line content so a println! reflow surfaces a remediation message.
//
// docsRegex anchoring: line 636 is the rollback_path bullet. The regex
// anchors on the settlement-unique prose `passes
// \`rollback_path.as_deref()\` through \`Option<&str>\`, and the
// unsuffixed CLI at \`main.rs:N-M\` maps \`None\` to the literal
// \`(none)\`` and captures both ranges. The sibling memory.backfill
// savepoint_name bullet at L651 uses `savepoint_name` (not
// `rollback_path`) and has a different shape (no rollback_path
// references at all), so first-match capture cannot drift, per the IPC
// docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const inlineEmissionAnchor = "rollback_path.as_deref(),";
const printBodyAnchor = 'rollback_path.as_deref().unwrap_or("(none)")';
const printFormatString = '"rollback_path: {}",';
const printCloser = ");";

const docsRegex =
  /The CLI's inline emission at `main\.rs:(\d+)` passes `rollback_path\.as_deref\(\)` through `Option<&str>`, and the unsuffixed CLI at `main\.rs:(\d+)-(\d+)` maps `None` to the literal `\(none\)`/;
const docsLabel = "settlement.backfill rollback_path inline-cite double citation";
const docsTemplate =
  "The CLI's inline emission at `main.rs:N1` passes `rollback_path.as_deref()` through `Option<&str>`, and the unsuffixed CLI at `main.rs:N2-M2` maps `None` to the literal `(none)`";

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

function findUniqueLine(lines, selector, label) {
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === selector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` (${label}) but found ${candidates.length}; remediation: confirm the anchor is present exactly once in the expected scope`,
    );
    return null;
  }
  return candidates[0];
}

function verifyLine(lines, lineNumber, expected, label) {
  const actual = lines[lineNumber - 1];
  if (actual === undefined || actual.trim() !== expected) {
    fail(
      `${sourcePath}:${lineNumber}: expected \`${expected}\` (${label}) but found \`${(actual ?? "").trim()}\`; remediation: the inline-cite convention requires this line to match the expected anchor — restore the structural shape or update this validator if the convention intentionally changed`,
    );
    return false;
  }
  return true;
}

let inlineLine = null;
let printRange = null;
if (source) {
  const lines = source.split("\n");

  inlineLine = findUniqueLine(
    lines,
    inlineEmissionAnchor,
    "inline emission anchor (settlement_backfill_json call arg)",
  );

  const printAnchorLine = findUniqueLine(
    lines,
    printBodyAnchor,
    "println! arg anchor (unsuffixed CLI rollback_path emission)",
  );
  if (printAnchorLine !== null) {
    const startLine = printAnchorLine - 1;
    const endLine = printAnchorLine + 1;
    const startOk = verifyLine(
      lines,
      startLine,
      printFormatString,
      "println! range start (format string)",
    );
    const endOk = verifyLine(
      lines,
      endLine,
      printCloser,
      "println! range end (closing `);`)",
    );
    if (startOk && endOk) {
      printRange = [startLine, endLine];
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the two inline cites in the rollback_path bullet sentence`,
    );
  } else if (inlineLine !== null && printRange !== null) {
    const citedInline = parseInt(match[1], 10);
    const citedPrint = [parseInt(match[2], 10), parseInt(match[3], 10)];
    if (citedInline !== inlineLine) {
      fail(
        `${docsPath}: the inline-emission cite is main.rs:${citedInline} but \`rollback_path.as_deref(),\` lives at :${inlineLine}; remediation: update the citation to :${inlineLine}`,
      );
    }
    if (
      citedPrint[0] !== printRange[0] ||
      citedPrint[1] !== printRange[1]
    ) {
      fail(
        `${docsPath}: the unsuffixed-CLI cite is main.rs:${citedPrint[0]}-${citedPrint[1]} but the rollback_path println! block lives at :${printRange[0]}-${printRange[1]}; remediation: update the citation to :${printRange[0]}-${printRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-settlement-backfill-rollback-path-inline-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-rollback-path-inline-line-refs: ok (inline-emission main.rs:${inlineLine}, unsuffixed-CLI main.rs:${printRange[0]}-${printRange[1]})`,
);
