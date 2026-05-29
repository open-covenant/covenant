#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// capability_list default limit (10) line-ref drift guard.
// docs/ipc-and-http-gateway.md line 248 cites a main.rs line for
// the `let mut limit: usize = 10;` declaration inside the
// `covenant capabilities recent` CLI verb body. The cite anchors
// the documented default (`(default \`10\`, see \`main.rs:N\`)`)
// to the source-of-truth so a refactor that shifts the verb body
// is caught at the docs-validator level — same drift class as the
// print-line cites corrected at commits 0d6c0b0f, d18aa198,
// a574f54e, 6ac85c66, though this slice targets a default-value
// declaration rather than a println! statement.
//
// Selector form: the single-line statement
// `let mut limit: usize = 10;` is NOT unique (4 sibling verbs use
// the same form: memory recent, memory search, receipt list, a2a
// status). The selector is disambiguated by requiring it to appear
// **immediately after** a `"recent" => {` match-arm opener AND to
// declare the default 10 (not 50, which the audit `recent` verb
// uses). The three `"recent" =>` siblings in main.rs are:
//
//   - memory recent at line 2075 — followed by `let mut tier:`
//   - capabilities recent at line 2561 — followed by `let mut limit: usize = 10;` (target)
//   - audit recent at line 3197 — followed by `let mut limit: usize = 50;`
//
// Only the capabilities subverb pairs `"recent" => {` with
// `let mut limit: usize = 10;` on the next line, making the pair
// a unique anchor.
//
// docsRegex anchoring: line 248 is one of five sibling `-n`/`--limit`
// bullets in the IPC docs (lines 198, 248, 293, 354, 437) that share
// the prose template `the request limit echoed back from
// \`-n\`/\`--limit\` (default \`N\`, X \`main.rs:M\`)`. Only the
// capability_list bullet uses `see` (not `per`) as the verb in the
// citation. Per the IPC docs pin collision anchor feedback, the
// regex anchors on the capability_list-specific combination of
// `default \`10\`, see \`main.rs:N\`` followed by the type-pin
// continuation `Pinned at the type level by the schema test
// (\`main.rs:M-K\`) — JSON consumers must never receive a string
// here`, which is unique to this bullet across the IPC docs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const armOpener = '"recent" => {';
const selector = "let mut limit: usize = 10;";

const docsRegex =
  /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, see `main\.rs:(\d+)`\)\. Pinned at the type level by the schema test \(`main\.rs:\d+-\d+`\) — JSON consumers must never receive a string here\./;
const docsLabel =
  "capability_list default-limit citation";
const docsTemplate =
  "- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, see `main.rs:N`). Pinned at the type level by the schema test (`main.rs:M-K`) — JSON consumers must never receive a string here.";

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
  for (let index = 0; index < lines.length - 1; index += 1) {
    if (
      lines[index].trim() === armOpener &&
      lines[index + 1].trim() === selector
    ) {
      candidates.push(index + 2);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${armOpener}\` immediately followed by \`${selector}\` but found ${candidates.length}; remediation: confirm the capability_list "recent" subverb still opens with the default-10 limit declaration on the next line`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the capability_list default-limit line`,
    );
  } else if (selectorLine !== null) {
    const citedLine = parseInt(match[1], 10);
    if (citedLine !== selectorLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${citedLine} but the default-limit declaration lives at :${selectorLine}; remediation: update the citation to :${selectorLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-capability-list-limit-default-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-capability-list-limit-default-line-refs: ok (default-limit main.rs:${selectorLine})`,
);
