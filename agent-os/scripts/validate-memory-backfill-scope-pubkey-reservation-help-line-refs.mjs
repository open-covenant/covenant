#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill --scope-pubkey reservation help/file-header
// double-citation line-ref drift guard. docs/ipc-and-http-gateway.md
// line 656 closes the memory scope-pubkey reservation paragraph with
// two inline cites describing where the not-yet-wired reservation is
// surfaced to operators:
//
//   * `main.rs:N1` — the `covenant help` usage line for
//     `memory backfill-receipt-correlation`, a single-line eprintln!,
//     carrying the `(--scope-pubkey reserved, not yet supported)` trailer.
//   * `main.rs:N2` — the file-header `//!` CLI-summary line for the
//     same verb.
//
// Mirror of validate-settlement-backfill-scope-pubkey-reservation-help-line-refs.mjs.
// Structural diff: the memory help line is a single-line eprintln! (the
// anchor includes the `eprintln!(...)` wrapper and trailing `;`),
// whereas settlement's help anchor was a standalone string literal
// inside a multi-line eprintln!.
//
// docsRegex collision risk (HIGH): L656 (memory) and L639 (settlement)
// share the IDENTICAL trailing phrase `but the daemon-side filter is
// not yet implemented (see the help text at \`main.rs:N\` and the
// file-header CLI summary at \`main.rs:M\`)` — only the line numbers
// differ. The regex anchors on the memory-unique variant name
// `Request::BackfillMemoryRecords.scope_pubkey` in the same sentence
// (with non-capturing \d+ for the already-pinned forwarding cites), so
// first-match capture always targets L656's help/file-header cites, not
// L639's, per the IPC docs pin collision anchor feedback.
//
// Anchors (each unique file-wide, verified by grep -cF):
//   * Help text: the trimmed single-line eprintln! at L1454.
//   * File header: the trimmed `//!` doc-comment line at L12.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const helpAnchor =
  'eprintln!("  covenant memory backfill-receipt-correlation [--dry-run] [--json]  apply legacy memory receipt correlation (--scope-pubkey reserved, not yet supported)");';
const fileHeaderAnchor =
  "//!   covenant memory backfill-receipt-correlation [--dry-run] [--json]   (--scope-pubkey reserved, not yet supported)";

const docsRegex =
  /Request::BackfillMemoryRecords\.scope_pubkey` \(`main\.rs:\d+-\d+`, `main\.rs:\d+`\), but the daemon-side filter is not yet implemented \(see the help text at `main\.rs:(\d+)` and the file-header CLI summary at `main\.rs:(\d+)`\)/;
const docsLabel =
  "memory.backfill --scope-pubkey reservation help/file-header double citation";
const docsTemplate =
  "..., but the daemon-side filter is not yet implemented (see the help text at `main.rs:N1` and the file-header CLI summary at `main.rs:N2`)";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` (${label}) but found ${candidates.length}; remediation: confirm the anchor is present exactly once — a reword that drops the \`--scope-pubkey reserved\` clause would break the docs' claim that the reservation is surfaced here`,
    );
    return null;
  }
  return candidates[0];
}

let helpLine = null;
let fileHeaderLine = null;
if (source) {
  const lines = source.split("\n");
  helpLine = findUniqueLine(lines, helpAnchor, "help-text usage line");
  fileHeaderLine = findUniqueLine(
    lines,
    fileHeaderAnchor,
    "file-header //! CLI-summary line",
  );
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the help-text and file-header cites in the memory --scope-pubkey reservation paragraph`,
    );
  } else if (helpLine !== null && fileHeaderLine !== null) {
    const citedHelp = parseInt(match[1], 10);
    const citedFileHeader = parseInt(match[2], 10);
    if (citedHelp !== helpLine) {
      fail(
        `${docsPath}: the help-text cite is main.rs:${citedHelp} but the usage line lives at :${helpLine}; remediation: update the citation to :${helpLine}`,
      );
    }
    if (citedFileHeader !== fileHeaderLine) {
      fail(
        `${docsPath}: the file-header cite is main.rs:${citedFileHeader} but the //! CLI-summary line lives at :${fileHeaderLine}; remediation: update the citation to :${fileHeaderLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-backfill-scope-pubkey-reservation-help-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-scope-pubkey-reservation-help-line-refs: ok (help text main.rs:${helpLine}, file header main.rs:${fileHeaderLine})`,
);
