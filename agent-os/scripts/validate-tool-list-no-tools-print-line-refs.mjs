#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_list "(no tools registered)" CLI fallback print line-ref drift
// guard. docs/ipc-and-http-gateway.md line 141 cites a main.rs line for
// the unsuffixed `covenant tools list` empty-array fallback println!.
// The cite points at the CLI handler arm in main.rs that runs when the
// daemon returns an empty `Response::ToolList`. This validator binds
// the docs prose to the source-of-truth so a refactor that shifts the
// CLI handler (a regular occurrence as new verbs are added above) is
// caught at the docs-validator level rather than left as silent drift.
//
// Selector form: the single-line statement
// `println!("(no tools registered)");` appears exactly once in main.rs
// — this is the only print of that literal across the CLI emitter
// surface. The selector is matched at top level (no test-fn brace
// scoping) because this is a CLI handler print, not a test assertion.
//
// docsRegex anchoring: line 141 contains TWO `main.rs:N` cites — one
// for the CLI print line and one for the `Pinned as an array by` type
// pin (`main.rs:6972-6975`). The regex anchors on the unique phrase
// `the unsuffixed CLI prints \`(no tools registered)\` for that case
// at \`main.rs:N\``, then closes before the period and the type pin
// sentence so it captures only the CLI-print cite. Per the IPC docs
// pin collision anchor feedback, the regex extends past the literal
// "(no tools registered)" with the unique "for that case at" trailer
// to prevent first-match contamination from the daemon-side
// `Response::ToolList { tools: vec![] }` empty-array test fixture or
// any future sibling bullet that uses the same empty-fallback prose.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = 'println!("(no tools registered)");';

const docsRegex =
  /the unsuffixed CLI prints `\(no tools registered\)` for that case at `main\.rs:(\d+)`\./;
const docsLabel =
  "tool_list (no tools registered) CLI fallback print citation";
const docsTemplate =
  "the unsuffixed CLI prints `(no tools registered)` for that case at `main.rs:N`.";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the tool_list empty-array CLI fallback print is present exactly once, not renamed or duplicated`,
    );
  } else {
    selectorLine = selectorMatches[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the (no tools registered) CLI fallback print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the (no tools registered) print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-tool-list-no-tools-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-list-no-tools-print-line-refs: ok (no-tools print main.rs:${selectorLine})`,
);
