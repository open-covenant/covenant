#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ignore_report unmatched-case CLI print line-ref drift guard.
// docs/ipc-and-http-gateway.md line 587 cites a main.rs line for
// the unsuffixed `covenant ignore check` unmatched-case println —
// the structural anchor that documents the human-readable
// rendering when no ignore rule matches. The cite is the
// `println!("not ignored ({rules_loaded} rule(s) loaded)");`
// statement inside the ignore CLI verb arm. Sibling to the
// matched-case validator that pins :4016. Without this validator
// a refactor that shifts the ignore arm body silently drifts
// the cite.
//
// Selector form: the single-line statement
// `println!("not ignored ({rules_loaded} rule(s) loaded)");` is
// unique in main.rs (verified by grep -c). The literal phrase
// "not ignored" followed by the `rules_loaded` interpolation is
// the disambiguating shape.
//
// docsRegex anchoring: line 587 carries TWO print cites in the
// same sentence — the matched-case cite (pinned by sibling
// validator) and the unmatched-case cite (target of this
// validator). The regex anchors on the "unmatched case prints"
// prefix and the full unique format-string placeholder phrase
// "not ignored (<n> rule(s) loaded)" so it captures only the
// unmatched-print cite, not the matched one. Per the IPC docs
// pin collision anchor feedback, the prefix + placeholder
// trailer defeats first-match contamination from the adjacent
// matched-case cite.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = 'println!("not ignored ({rules_loaded} rule(s) loaded)");';

const docsRegex =
  /the unmatched case prints `not ignored \(<n> rule\(s\) loaded\)` at `main\.rs:(\d+)`/;
const docsLabel =
  "ignore_report unmatched-case print citation";
const docsTemplate =
  "the unmatched case prints `not ignored (<n> rule(s) loaded)` at `main.rs:N`";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` but found ${candidates.length}; remediation: confirm the unmatched-case ignore CLI print still uses this exact statement, not refactored or duplicated`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the unmatched-case ignore CLI print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the unmatched-case print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-ignore-report-unmatched-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-ignore-report-unmatched-print-line-refs: ok (unmatched-case print main.rs:${selectorLine})`,
);
