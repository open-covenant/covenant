#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory.backfill verb-name-asymmetry match-arm cite line-ref drift
// guard. docs/ipc-and-http-gateway.md line 654 (the verb-name-asymmetry
// paragraph) carries a standalone cite `The match arm is at
// main.rs:N` justifying why only the long verb
// `memory backfill-receipt-correlation` parses — the shorter
// `memory backfill` / `memory backfill-receipts` spellings bail with
// `unknown flag`. The cited line is the dispatch-arm opener
// `"backfill-receipt-correlation" => {`.
//
// Relationship to the sibling cli-wiring validator: validate-memory-
// backfill-cli-wiring-line-refs.mjs uses the SAME opener anchor but
// binds it to the L660 `The CLI verb is wired at main.rs:N-M` RANGE
// sentence (opener through arm close, 2350-2401). This validator binds
// the L654 single-line cite (the opener line only). Both reference the
// same start line but live in different docs sentences with disjoint
// docsRegex, so they coexist.
//
// docsRegex anchoring: the phrase `The match arm is at main.rs:N` is
// unique to L654; no settlement sibling has a verb-name-asymmetry
// paragraph (settlement uses the shorter verb with no asymmetry). The
// regex anchors on `The match arm is at \`main.rs:(N)\`; the shorter
// spellings do not parse` — the trailing `shorter spellings do not
// parse` clause is L654-unique, so first-match capture cannot drift,
// per the IPC docs pin collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const matchArmAnchor = '"backfill-receipt-correlation" => {';

const docsRegex =
  /The match arm is at `main\.rs:(\d+)`; the shorter spellings do not parse/;
const docsLabel = "memory.backfill verb-name-asymmetry match-arm citation";
const docsTemplate =
  "The match arm is at `main.rs:N`; the shorter spellings do not parse";

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

let matchArmLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === matchArmAnchor) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${matchArmAnchor}\` but found ${candidates.length}; remediation: confirm the memory backfill-receipt-correlation dispatch arm is present exactly once in the memory subcommand match`,
    );
  } else {
    matchArmLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the match-arm cite in the verb-name-asymmetry paragraph`,
    );
  } else if (matchArmLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== matchArmLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the match-arm opener lives at :${matchArmLine}; remediation: update the citation to :${matchArmLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-memory-backfill-verb-asymmetry-match-arm-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-backfill-verb-asymmetry-match-arm-line-refs: ok (match arm main.rs:${matchArmLine})`,
);
