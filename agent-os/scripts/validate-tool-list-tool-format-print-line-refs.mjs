#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// tool_list per-tool format CLI print line-ref drift guard.
// docs/ipc-and-http-gateway.md line 153 cites a main.rs line for
// the unsuffixed `covenant tools list` per-tool format println!
// that emits `<name> — <description>` on its own line when at
// least one tool is registered. The cite points at the CLI handler
// arm in main.rs. This validator binds the docs prose to the
// source-of-truth so a refactor that shifts the CLI handler is
// caught at the docs-validator level — same drift class as the
// earlier tool_list `(no tools registered)`, capability_revoke
// `(no live capability with that signature)`, and a2a_status
// `(a2a queue empty)` print-line cites corrected at commits
// 0d6c0b0f, d18aa198, and a574f54e.
//
// Selector form: the single-line statement
// `println!("{} — {}", t.name, t.description);` appears exactly
// once in main.rs. The selector is matched at top level (no
// test-fn brace scoping) because this is a CLI handler print, not
// a test assertion.
//
// docsRegex anchoring: line 153's source-of-truth sentence
// contains multiple `main.rs:N` cites — the helper-fn cite
// (`main.rs:4495`), the renders-test cite (`main.rs:6931`), the
// pins-test cite (`main.rs:6955`), the CLI verb wiring range
// (`main.rs:3107-3133`), and the per-tool format print cite (the
// target of this validator). The regex anchors on the unique
// phrase `the same response prints one line per tool in the form
// \`<name> — <description>\` at \`main.rs:N\`` to capture only the
// per-tool format print cite. Per the IPC docs pin collision
// anchor feedback, the regex extends past the unique literal
// `<name> — <description>` with the "at" cite slot to prevent
// first-match contamination from the surrounding cites.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = 'println!("{} — {}", t.name, t.description);';

const docsRegex =
  /the same response prints one line per tool in the form `<name> — <description>` at `main\.rs:(\d+)`\./;
const docsLabel =
  "tool_list per-tool format CLI print citation";
const docsTemplate =
  "the same response prints one line per tool in the form `<name> — <description>` at `main.rs:N`.";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the tool_list per-tool format CLI print is present exactly once, not renamed or duplicated`,
    );
  } else {
    selectorLine = selectorMatches[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the per-tool format CLI print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the per-tool format print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-tool-list-tool-format-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-tool-list-tool-format-print-line-refs: ok (tool-format print main.rs:${selectorLine})`,
);
