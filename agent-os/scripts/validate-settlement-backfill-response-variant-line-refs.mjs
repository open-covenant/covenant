#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// settlement.backfill Response variant range line-ref drift
// guard. docs/ipc-and-http-gateway.md line 643 cites a five-line
// main.rs range for the Response::SettlementReceiptsBackfilled
// destructuring pattern in the CLI handler. The cited range
// starts at the variant opener `Response::SettlementReceiptsBackfilled {`
// and ends at the arm-opener brace `} => {` after the three
// destructured field bindings (row_count, rollback_path, dry_run).
// The validator pins the range via the unique variant-name anchor
// and verifies the structural layout of the destructuring block,
// so a refactor that, e.g., reorders fields or destructures by
// reference fails at validate time.
//
// Opener selector: the trimmed line
// `Response::SettlementReceiptsBackfilled {` appears exactly once
// in main.rs (verified by grep -nF). The validator asserts
// candidates==1 so a future duplicate site (unlikely but possible
// if a refactor adds a separate sync handler) surfaces the
// duplication.
//
// docsRegex anchoring: line 643 contains four main.rs cites
// (CLI wiring + 3 internal range cites + helper/pins-test/renders
// covered by sibling validators). The regex anchors on the unique
// variant name `Response::SettlementReceiptsBackfilled` in the
// daemon-side sentence so first-match capture targets only this
// cite, per the IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = "Response::SettlementReceiptsBackfilled {";
// Layout from the opener line (offset = lineNumber - openerLine):
//    0  Response::SettlementReceiptsBackfilled {   <-- opener
//   +1  row_count,
//   +2  rollback_path,
//   +3  dry_run,
//   +4  } => {
const expectedShape = [
  { offset: 1, content: "row_count," },
  { offset: 2, content: "rollback_path," },
  { offset: 3, content: "dry_run," },
  { offset: 4, content: "} => {" },
];

const docsRegex =
  /The daemon-side `Response::SettlementReceiptsBackfilled` variant carries the three fields directly \(`main\.rs:(\d+)-(\d+)`\)/;
const docsLabel =
  "settlement.backfill Response variant range citation";
const docsTemplate =
  "The daemon-side `Response::SettlementReceiptsBackfilled` variant carries the three fields directly (`main.rs:N-M`)";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${opener}\` but found ${candidates.length}; remediation: confirm the Response::SettlementReceiptsBackfilled destructuring is present exactly once in the CLI handler`,
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
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the Response::SettlementReceiptsBackfilled variant range`,
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
    "validate-settlement-backfill-response-variant-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-settlement-backfill-response-variant-line-refs: ok (Response variant main.rs:${startLine}-${endLine})`,
);
