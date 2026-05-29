#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill Response variant range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 660 cites a five-line main.rs
// range for the Response::MemoryRecordsBackfilled destructuring
// pattern in the CLI handler. The cited range starts at the
// variant opener `Response::MemoryRecordsBackfilled {` and ends
// at the arm-opener brace `} => {` after the three destructured
// field bindings (row_count, savepoint_name, dry_run).
//
// Mirror of validate-settlement-backfill-response-variant-line-refs.mjs.
// Same destructuring shape and structural verification approach;
// the second field is `savepoint_name` instead of `rollback_path`,
// matching the envelope-layer field-shape diff documented at
// docs L646 (rollback_path is string-or-null, savepoint_name is
// always a non-null non-empty string).
//
// Opener selector: the trimmed line
// `Response::MemoryRecordsBackfilled {` appears exactly once in
// main.rs (verified by grep -nF). The validator asserts
// candidates==1.
//
// docsRegex anchoring: line 660 contains four main.rs cites. The
// regex anchors on the unique variant name
// `Response::MemoryRecordsBackfilled` in the daemon-side sentence
// so first-match capture targets only this cite, per the IPC
// docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = "Response::MemoryRecordsBackfilled {";
// Layout from the opener line (offset = lineNumber - openerLine):
//    0  Response::MemoryRecordsBackfilled {   <-- opener
//   +1  row_count,
//   +2  savepoint_name,
//   +3  dry_run,
//   +4  } => {
const expectedShape = [
  { offset: 1, content: "row_count," },
  { offset: 2, content: "savepoint_name," },
  { offset: 3, content: "dry_run," },
  { offset: 4, content: "} => {" },
];

const docsRegex =
  /The daemon-side `Response::MemoryRecordsBackfilled` variant carries the three fields directly \(`main\.rs:(\d+)-(\d+)`\)/;
const docsLabel =
  "memory.backfill Response variant range citation";
const docsTemplate =
  "The daemon-side `Response::MemoryRecordsBackfilled` variant carries the three fields directly (`main.rs:N-M`)";

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
    if (lines[index].trim() === opener) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${opener}\` but found ${candidates.length}; remediation: confirm the Response::MemoryRecordsBackfilled destructuring is present exactly once in the CLI handler`,
    );
  } else {
    const openerLine = candidates[0];
    let shapeOk = true;
    for (const expected of expectedShape) {
      const targetLine = openerLine + expected.offset;
      const actual = lines[targetLine - 1];
      if (actual === undefined || actual.trim() !== expected.content) {
        fail(
          `${sourcePath}:${targetLine}: expected \`${expected.content}\` (offset ${expected.offset} from variant opener) but found \`${(actual ?? "").trim()}\`; remediation: the five-line destructuring convention requires this layout — restore the structural shape or update this validator if the convention intentionally changed`,
        );
        shapeOk = false;
      }
    }
    if (shapeOk) {
      startLine = openerLine;
      endLine = openerLine + 4;
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the Response::MemoryRecordsBackfilled variant range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the variant destructuring lives at :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-backfill-response-variant-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-response-variant-line-refs: ok (Response variant main.rs:${startLine}-${endLine})`,
);
