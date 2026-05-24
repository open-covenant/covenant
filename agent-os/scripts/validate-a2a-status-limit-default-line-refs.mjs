#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// a2a_status default limit (10) line-ref drift guard.
// docs/ipc-and-http-gateway.md line 437 cites a main.rs line for
// the `let mut limit: usize = 10;` declaration inside the
// `covenant a2a status` CLI verb body. The cite anchors the
// documented default (`(default \`10\`, per \`main.rs:N\`)`) to
// the source-of-truth so a refactor that shifts the verb body is
// caught at the docs-validator level — same drift class as the
// capability_list (commit a1192c55) and peer_list (commit
// a2d78ad8) default-limit cite fixes.
//
// Selector form: the single-line statement
// `let mut limit: usize = 10;` is NOT unique (5 sibling verbs use
// the same form). Disambiguation: require the line above to be
// `"status" => {` (the a2a status subverb opener). The
// `"status" => {` opener appears twice in main.rs — once in
// chain status at line 2885 followed by `let mut as_json = false;`,
// and once in a2a status at line 3349 followed by
// `let mut limit: usize = 10;` (target). Only the a2a status
// subverb pairs `"status" => {` with `let mut limit: usize = 10;`
// on the next line, making the pair a unique anchor. Mirrors the
// adjacency strategy used by
// validate-capability-list-limit-default-line-refs.mjs.
//
// docsRegex anchoring: line 437 is one of five sibling `-n`/`--limit`
// bullets in the IPC docs (lines 198, 248, 293, 354, 437). The
// a2a_status bullet is unique in pairing `default \`10\`, per
// \`main.rs:N\`` with the trailing schema-test cite formatted as
// `Pinned as u64 by the schema test (\`main.rs:M-K\`).` and NO
// "— never X" clarifier (every sibling -n/--limit bullet has
// some "— never X" trailer). Per the IPC docs pin collision anchor
// feedback, the regex anchors on the unique no-trailer form to
// prevent first-match contamination from sibling bullets.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const armOpener = '"status" => {';
const selector = "let mut limit: usize = 10;";

const docsRegex =
  /- `limit` \(u64\): the request limit echoed back from `-n`\/`--limit` \(default `10`, per `main\.rs:(\d+)`\)\. Pinned as u64 by the schema test \(`main\.rs:\d+-\d+`\)\.\n/;
const docsLabel = "a2a_status default-limit citation";
const docsTemplate =
  "- `limit` (u64): the request limit echoed back from `-n`/`--limit` (default `10`, per `main.rs:N`). Pinned as u64 by the schema test (`main.rs:M-K`).";

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
      `${sourcePath}: expected exactly 1 occurrence of \`${armOpener}\` immediately followed by \`${selector}\` but found ${candidates.length}; remediation: confirm the a2a status subverb still opens with the default-10 limit declaration on the next line`,
    );
  } else {
    selectorLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the citation that records the a2a_status default-limit line`,
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
  console.error("validate-a2a-status-limit-default-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-a2a-status-limit-default-line-refs: ok (default-limit main.rs:${selectorLine})`,
);
