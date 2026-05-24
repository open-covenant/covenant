#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability grant expand-action stderr-line line-ref drift guard.
// docs/ipc-and-http-gateway.md line 261 documents the `action` field of
// the capability-granted envelope and notes that when the CLI receives an
// a2a peer-prefix shorthand it prints an `expanding <prefix> → <full>`
// line to stderr. That cite drifted (the docs said main.rs:2680 while the
// eprintln had moved to main.rs:2673) and went unnoticed because the
// aggregate validate-capability-grant-type-level-pin-line-refs.mjs matches
// the whole L261 sentence with a non-capturing `\d+` for this cite and
// only asserts the trailing `Pinned as a string by` type cite.
//
// This validator binds the cite directly: it anchors on the unique
// `eprintln!("expanding {prefix} → {full}");` statement (grep -F count 1)
// and captures the cited line from the L261-unique
// `expanding <prefix> → <full>` line to stderr at `main.rs:N` phrase. The
// two validators read the docs independently and assert disjoint cites,
// so they coexist without collision.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const eprintlnAnchor = 'eprintln!("expanding {prefix} → {full}");';

const docsRegex =
  /`expanding <prefix> → <full>` line to stderr at `main\.rs:(\d+)`/;
const docsLabel = "expand-action stderr-line citation";
const docsTemplate =
  "`expanding <prefix> → <full>` line to stderr at `main.rs:N`";

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

let eprintlnLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === eprintlnAnchor) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${eprintlnAnchor}\` but found ${candidates.length}; remediation: confirm the a2a-action expansion print is present exactly once on the capability grant path`,
    );
  } else {
    eprintlnLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the stderr-line cite in the capability grant action bullet`,
    );
  } else if (eprintlnLine !== null) {
    const cited = parseInt(match[1], 10);
    if (cited !== eprintlnLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited} but the eprintln lives at :${eprintlnLine}; remediation: update the citation to :${eprintlnLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-capability-grant-expand-action-stderr-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-grant-expand-action-stderr-line-refs: ok (eprintln main.rs:${eprintlnLine})`,
);
