#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ignore_report exit-coupling block line-ref drift guard.
// docs/ipc-and-http-gateway.md line 583 carries the
// **Exit-code coupling** sentence for the ignore_report envelope:
// "when `ignored` is `true`, the CLI exits `1` even in the
// `--json` path (per `main.rs:N-M`)". The cited range is the
// three-line block that early-exits the ignore CLI verb when the
// daemon-evaluated `ignored` field is true — the structural
// anchor that documents the "exit 1 is the expected signal for
// a matched ignore rule, not an error" contract.
//
// Selector form: the three-line block (opener `if ignored {`,
// body `std::process::exit(1);`, closer `}`) is unique in
// main.rs as a triple. The single-line opener `if ignored {`
// appears once at the standalone-if shape (the only other
// "ignored" branch in the same arm uses `} else if ignored {`,
// which has the `} else` prefix and is therefore disjoint), but
// the validator matches the triple to defend against a refactor
// that keeps the opener but changes the body.
//
// docsRegex anchoring: per the IPC docs pin collision anchor
// feedback, the regex captures the unique exit-coupling phrase
// "even in the `--json` path (per `main.rs:N-M`)". This phrase
// is distinct from the intents_resume exit-coupling sentence at
// line 624 ("the `--json` path exits `1` on every error branch
// (`main.rs:N`, `main.rs:N`, `main.rs:N`)" — sibling-list form,
// not the parenthetical "per" form). The `per ` prefix and the
// range form together fix the match to this sentence.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const opener = "if ignored {";
const body = "std::process::exit(1);";
const closer = "}";

const docsRegex =
  /even in the `--json` path \(per `main\.rs:(\d+)-(\d+)`\)/;
const docsLabel =
  "ignore_report exit-coupling citation";
const docsTemplate =
  "even in the `--json` path (per `main.rs:N-M`)";

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

let openerLine = null;
let closerLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length - 2; index += 1) {
    if (
      lines[index].trim() === opener &&
      lines[index + 1].trim() === body &&
      lines[index + 2].trim() === closer
    ) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of the triple-line block (\`${opener}\`, \`${body}\`, \`${closer}\`) but found ${candidates.length}; remediation: confirm the ignore_report exit-coupling block still uses this exact three-line shape, not refactored or duplicated`,
    );
  } else {
    openerLine = candidates[0];
    closerLine = openerLine + 2;
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the ignore_report exit-coupling block`,
    );
  } else if (openerLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== openerLine || citedEnd !== closerLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the exit-coupling block lives at :${openerLine}-${closerLine}; remediation: update the citation to :${openerLine}-${closerLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-ignore-report-exit-code-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ignore-report-exit-code-line-refs: ok (exit-coupling block main.rs:${openerLine}-${closerLine})`,
);
