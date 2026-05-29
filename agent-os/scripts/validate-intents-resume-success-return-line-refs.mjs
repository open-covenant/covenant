#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume success-branch normal-return line-ref drift
// guard. docs/ipc-and-http-gateway.md line 624 cites a main.rs
// line for the `return Ok(());` statement that fires after the
// intents_resume_ok_json envelope is printed in the --json
// success path. The cite documents the "success exits 0 via
// normal return" contract — the structural counterpart to the
// three error-branch exit-1 cites in the same sentence.
//
// Selector form: bare `return Ok(());` is not unique in main.rs
// (4 occurrences). The structural disambiguator is the parent
// helper call `serde_json::to_string(&intents_resume_ok_json(`
// which is unique (only inline helper-call site outside the fn
// declaration and test bodies). The validator finds that line
// then searches forward within a bounded window (12 lines) for
// the next `return Ok(());` — defends against helper-arg
// reordering or wrap changes without false-matching a later
// return.
//
// docsRegex anchoring: line 624 carries the success-return cite
// alongside THREE sibling error-branch cites in the same
// sentence ("the `--json` path exits `1` on every error branch
// (`main.rs:N`, `main.rs:N`, `main.rs:N`); ...; Success exits
// `0` via the normal return at `main.rs:N`"). The regex anchors
// on the "Success exits `0` via the normal return at" prefix to
// capture only the success-return cite, not the three error
// cites. Per the IPC docs pin collision anchor feedback, the
// full success-prefix phrase is the unique anchor.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const helperCall = "serde_json::to_string(&intents_resume_ok_json(";
const returnStatement = "return Ok(());";
const searchWindow = 12;

const docsRegex =
  /Success exits `0` via the normal return at `main\.rs:(\d+)`/;
const docsLabel =
  "intents_resume success-branch normal-return citation";
const docsTemplate =
  "Success exits `0` via the normal return at `main.rs:N`";

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

let returnLine = null;
if (source) {
  const lines = source.split("\n");
  const helperMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === helperCall) {
      helperMatches.push(index + 1);
    }
  }
  if (helperMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 inline occurrence of \`${helperCall}\` but found ${helperMatches.length}; remediation: confirm the intents_resume success-branch helper call is still made at this exact site`,
    );
  } else {
    const anchor = helperMatches[0];
    for (let index = anchor; index < Math.min(anchor + searchWindow, lines.length); index += 1) {
      if (lines[index].trim() === returnStatement) {
        returnLine = index + 1;
        break;
      }
    }
    if (returnLine === null) {
      fail(
        `${sourcePath}: could not locate \`${returnStatement}\` within ${searchWindow} lines below the helper call at line ${anchor}; remediation: confirm the success-branch return follows the envelope print within the expected window`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the success-branch normal-return line`,
    );
  } else if (returnLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== returnLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the success-branch return lives at :${returnLine}; remediation: update the citation to :${returnLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-intents-resume-success-return-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-success-return-line-refs: ok (success-branch return main.rs:${returnLine})`,
);
