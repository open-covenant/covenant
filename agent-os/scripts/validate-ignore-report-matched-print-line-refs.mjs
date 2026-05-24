#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ignore_report matched-case CLI print line-ref drift guard.
// docs/ipc-and-http-gateway.md line 587 cites a main.rs line for
// the unsuffixed `covenant ignore check` matched-case println —
// the structural anchor that documents the human-readable
// rendering of a matched ignore rule. The cite is the
// `println!("ignored — matched rule: {pat}");` statement inside
// the ignore CLI verb arm. Without this validator, a refactor
// that shifts the ignore arm body silently drifts the cite.
//
// Selector form: the single-line statement
// `println!("ignored — matched rule: {pat}");` is unique in
// main.rs (verified by grep -c). The em dash (—, U+2014) is the
// disambiguating character — no other println uses the literal
// phrase "ignored — matched rule".
//
// docsRegex anchoring: line 587 contains TWO print cites in the
// same sentence — the matched-case cite (target of this
// validator) and the sibling unmatched-case cite ("the unmatched
// case prints `not ignored (<n> rule(s) loaded)` at
// `main.rs:M`"). The regex anchors on the "matched case prints"
// prefix and the full unique format-string placeholder phrase
// "ignored — matched rule: <pattern>" so it captures only the
// matched-print cite, not the unmatched one. Per the IPC docs
// pin collision anchor feedback, the prefix + placeholder
// trailer is sufficient to defeat first-match contamination
// from the adjacent unmatched-case cite.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = 'println!("ignored — matched rule: {pat}");';

const docsRegex =
  /the matched case prints `ignored — matched rule: <pattern>` at `main\.rs:(\d+)`/;
const docsLabel =
  "ignore_report matched-case print citation";
const docsTemplate =
  "the matched case prints `ignored — matched rule: <pattern>` at `main.rs:N`";

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

let selectorLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === selector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` but found ${candidates.length}; remediation: confirm the matched-case ignore CLI print still uses this exact statement, not refactored or duplicated`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the matched-case ignore CLI print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the matched-case print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-ignore-report-matched-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ignore-report-matched-print-line-refs: ok (matched-case print main.rs:${selectorLine})`,
);
