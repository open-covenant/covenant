#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peer_list default limit (20) line-ref drift guard.
// docs/ipc-and-http-gateway.md line 293 cites a main.rs line for
// the `let mut limit: usize = 20;` declaration inside the
// `covenant peers list` CLI verb body. The cite anchors the
// documented default (`(default \`20\`, per \`main.rs:N\`)`) to
// the source-of-truth so a refactor that shifts the verb body is
// caught at the docs-validator level — same drift class as the
// capability_list default-limit cite corrected at commit
// a1192c55.
//
// Selector form: the single-line statement
// `let mut limit: usize = 20;` is UNIQUE inside main.rs — peer_list
// is the only CLI verb using default 20 (memory recent/search,
// capability_list, receipt_list, and a2a_status all default to 10;
// audit_recent defaults to 50). No brace-scoping is needed; a
// top-level exact-trim-match isolates the peer_list match
// unambiguously.
//
// docsRegex anchoring: line 293 is one of five sibling `-n`/`--limit`
// bullets in the IPC docs (lines 198, 248, 293, 354, 437) that
// share the prose template `the request limit echoed back from
// \`-n\`/\`--limit\` (default \`N\`, X \`main.rs:M\`)`. Only the
// peer_list bullet uses `default \`20\`` (siblings use 10 or 50).
// Additionally peer_list is the only bullet using `--limit` (no
// `-n` alias) since the peers-list verb does not accept `-n`. Per
// the IPC docs pin collision anchor feedback, the regex anchors on
// the peer_list-specific `default \`20\`, per \`main.rs:N\`` plus
// the unique type-pin continuation `Pinned as u64 by \`main.rs:M-K\`
// — never a string.`, which is unique to this bullet across the IPC
// docs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const selector = "let mut limit: usize = 20;";

const docsRegex =
  /- `limit` \(u64\): the request limit echoed back from `--limit` \(default `20`, per `main\.rs:(\d+)`\)\. Pinned as u64 by `main\.rs:\d+-\d+` — never a string\./;
const docsLabel = "peer_list default-limit citation";
const docsTemplate =
  "- `limit` (u64): the request limit echoed back from `--limit` (default `20`, per `main.rs:N`). Pinned as u64 by `main.rs:M-K` — never a string.";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${selector}\` at top level but found ${selectorMatches.length}; remediation: confirm the peer_list CLI verb still declares the default-20 limit exactly once and no sibling verb introduced a default-20`,
    );
  } else {
    selectorLine = selectorMatches[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the peer_list default-limit line`,
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
  console.error("validate-peer-list-limit-default-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peer-list-limit-default-line-refs: ok (default-limit main.rs:${selectorLine})`,
);
