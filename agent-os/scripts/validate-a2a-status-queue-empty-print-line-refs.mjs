#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_status "(a2a queue empty)" CLI fallback print line-ref drift
// guard. docs/ipc-and-http-gateway.md line 442 cites a main.rs line
// for the unsuffixed `covenant a2a status` empty-queue fallback
// println!. The cite points at the CLI handler arm that runs when
// both `tasks` and `results` are empty. This validator binds the
// docs prose to the source-of-truth so a refactor that shifts the
// CLI handler (which happens as sibling subcommands are added above)
// is caught at the docs-validator level — same drift class as the
// tool_list and capability_revoke print-line cites corrected at
// commits 0d6c0b0f and d18aa198.
//
// Selector form: the single-line statement
// `println!("(a2a queue empty)");` appears exactly once in main.rs.
// The selector is matched at top level (no test-fn brace scoping)
// because this is a CLI handler print, not a test assertion.
//
// docsRegex anchoring: line 442 contains another `main.rs:N-M` cite
// (the type-level pin `Pinned as an array by main.rs:7421-7424`).
// The regex anchors on the unique phrase `the unsuffixed CLI prints
// \`(a2a queue empty)\` at \`main.rs:N\` when both \`tasks\` and
// \`results\` are empty`, capturing only the fallback-print cite.
// Per the IPC docs pin collision anchor feedback, the regex extends
// past the unique literal `(a2a queue empty)` with the "when both"
// trailer to prevent first-match contamination from any sibling
// bullet that uses similar empty-array fallback prose.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = 'println!("(a2a queue empty)");';

const docsRegex =
  /the unsuffixed CLI prints `\(a2a queue empty\)` at `main\.rs:(\d+)` when both `tasks` and `results` are empty\./;
const docsLabel =
  "a2a_status (a2a queue empty) CLI fallback print citation";
const docsTemplate =
  "the unsuffixed CLI prints `(a2a queue empty)` at `main.rs:N` when both `tasks` and `results` are empty.";

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
  const selectorMatches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === selector) {
      selectorMatches.push(index + 1);
    }
  }
  if (selectorMatches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the a2a_status empty-queue CLI fallback print is present exactly once, not renamed or duplicated`,
    );
  } else {
    selectorLine = selectorMatches[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the (a2a queue empty) CLI fallback print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the (a2a queue empty) print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-a2a-status-queue-empty-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-queue-empty-print-line-refs: ok (queue-empty print main.rs:${selectorLine})`,
);
