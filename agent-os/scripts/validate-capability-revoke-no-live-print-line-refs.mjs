#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_revoke "(no live capability with that signature)" CLI
// fallback print line-ref drift guard. docs/ipc-and-http-gateway.md
// line 274 cites a main.rs line for the unsuffixed CLI's
// false-removed message. The cite points at the CLI handler arm in
// main.rs that runs when the daemon returns `removed=false`. This
// validator binds the docs prose to the source-of-truth so a refactor
// that shifts the CLI handler (which happens as sibling subcommands
// are added above) is caught at the docs-validator level rather than
// left as silent drift — the same drift class as the tool_list
// "(no tools registered)" cite corrected at commit 0d6c0b0f.
//
// Selector form: the single-line statement
// `println!("(no live capability with that signature)");` appears
// exactly once in main.rs. The selector is matched at top level (no
// test-fn brace scoping) because this is a CLI handler print, not a
// test assertion.
//
// docsRegex anchoring: line 274 contains MULTIPLE `main.rs:N` cites
// — one for the type-level pin (`main.rs:6066-6069`), one for the
// fallback print (target of this validator), and one for the unsuffixed
// CLI print line (`main.rs:2769`). The regex anchors on the unique
// phrase `the unsuffixed CLI prints \`(no live capability with that
// signature)\` for that case at \`main.rs:N\``, capturing only the
// fallback-print cite. Per the IPC docs pin collision anchor feedback,
// the regex extends past the unique literal `(no live capability with
// that signature)` with the "for that case at" trailer to prevent
// first-match contamination from any sibling bullet that uses the
// same empty-fallback prose template.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector =
  'println!("(no live capability with that signature)");';

const docsRegex =
  /the unsuffixed CLI prints `\(no live capability with that signature\)` for that case at `main\.rs:(\d+)`\./;
const docsLabel =
  "capability_revoke (no live capability with that signature) CLI fallback print citation";
const docsTemplate =
  "the unsuffixed CLI prints `(no live capability with that signature)` for that case at `main.rs:N`.";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the capability_revoke false-removed CLI fallback print is present exactly once, not renamed or duplicated`,
    );
  } else {
    selectorLine = selectorMatches[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the (no live capability with that signature) CLI fallback print line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the (no live capability with that signature) print lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-capability-revoke-no-live-print-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-revoke-no-live-print-line-refs: ok (no-live print main.rs:${selectorLine})`,
);
