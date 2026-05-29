#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume CLI sources block print range line-ref drift
// guard. docs/ipc-and-http-gateway.md line 614 cites a main.rs
// range for the four-line CLI block that prints the `sources:`
// header and each `  - <label>` line on the success branch of
// `covenant intents resume` (unsuffixed path). The cited range
// covers `println!("sources:");` + the for-loop opener + the
// per-label body print + the for-loop closer.
//
// Selector form: the trimmed line `println!("sources:");`
// appears exactly once in main.rs (verified by grep -nF). The
// validator asserts candidates==1 so a refactor that introduces
// a second matching header surfaces the duplication instead of
// silently picking the first match.
//
// Range derivation: from the matched selector line N, the next
// three lines must trim to `for s in sources {`,
// `println!("  - {s}");`, and `}` respectively. If any line
// drifts, the validator fails with a remediation message that
// names the unexpected content, so a structural refactor never
// gets silently re-pinned to the wrong range.
//
// docsRegex anchoring: line 614 carries TWO cites — the
// type-level sources Pinned-as range (5385-5388, already covered
// by validate-intents-resume-type-level-pin-line-refs.mjs's
// `sources_in_ok_branch` target) and this CLI print range. The
// regex anchors on the unique trailing phrase
// "the unsuffixed CLI prints a `sources:` block followed by
// `  - <label>` lines at `main.rs:N-M`" so first-match capture
// targets only the CLI print cite, per the IPC docs pin
// collision anchor feedback.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const headerSelector = 'println!("sources:");';
const expectedFollowing = [
  "for s in sources {",
  'println!("  - {s}");',
  "}",
];

const docsRegex =
  /the unsuffixed CLI prints a `sources:` block followed by `  - <label>` lines at `main\.rs:(\d+)-(\d+)` only when the array is non-empty\./;
const docsLabel =
  "intents_resume CLI sources block print range citation";
const docsTemplate =
  "the unsuffixed CLI prints a `sources:` block followed by `  - <label>` lines at `main.rs:N-M` only when the array is non-empty.";

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
    if (lines[index].trim() === headerSelector) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${headerSelector}\` but found ${candidates.length}; remediation: confirm the intents_resume CLI sources block header is present exactly once on the success path`,
    );
  } else {
    startLine = candidates[0];
    for (let offset = 0; offset < expectedFollowing.length; offset += 1) {
      const expectedLineNum = startLine + 1 + offset;
      const actual = lines[expectedLineNum - 1];
      if (actual === undefined || actual.trim() !== expectedFollowing[offset]) {
        fail(
          `${sourcePath}:${expectedLineNum}: expected \`${expectedFollowing[offset]}\` but found \`${(actual ?? "").trim()}\`; remediation: the four-line sources block convention requires header, for-opener, body print, for-closer on consecutive lines — restore the structural shape or update this validator if the convention intentionally changed`,
        );
      }
    }
    endLine = startLine + expectedFollowing.length;
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the intents_resume CLI sources block print range`,
    );
  } else if (startLine !== null && endLine !== null && errors.length === 0) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedStart}-${citedEnd} but the sources block lives at :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-cli-sources-print-range-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-cli-sources-print-range-line-refs: ok (CLI sources block main.rs:${startLine}-${endLine})`,
);
