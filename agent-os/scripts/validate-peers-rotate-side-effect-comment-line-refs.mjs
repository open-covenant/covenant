#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// peers_rotate token-persistence side-effect comment line-ref drift
// guard. docs/ipc-and-http-gateway.md line 328 (the peers rotate
// `Side effects before the envelope returns` sentence) cites the CLI
// comment block at main.rs:3677-3683 — the inline explanation that the
// daemon has already written the new token to
// `$COVENANT_HOME/peers/operator.token` (mode 0600) before the rotate
// envelope returns, so the printed envelope is informational.
//
// No prior validator bound this cite: validate-peers-rotate-line-refs.mjs
// guards the `peers_rotate_json` envelope helper and its two shape tests
// via the "source-of-truth" sentence, not this side-effect comment.
//
// The block is a run of consecutive `//` lines. The validator anchors on
// the unique opener `// The daemon already wrote the new token to`
// (grep -F count 1) and walks forward while lines stay comments; the
// last consecutive comment line is the closer (the block is terminated
// by `if as_json {`). A reword, reflow, or split of the comment shifts
// the derived range and surfaces a remediation message — the intended
// drift signal.
//
// docsRegex anchoring: `Side effects before the envelope returns` is
// unique to line 328, so first-match capture cannot drift to another
// envelope sentence. The cite is a `main.rs:N-M` range and both
// endpoints are captured.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const commentOpener = "// The daemon already wrote the new token to";
const commentWindowLines = 40;

const docsRegex =
  /Side effects before the envelope returns \(per the CLI comment at `main\.rs:(\d+)-(\d+)`\)/;
const docsLabel = "peers_rotate side-effect comment range citation";
const docsTemplate =
  "Side effects before the envelope returns (per the CLI comment at `main.rs:N-M`)";

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

let commentRange = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === commentOpener) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${commentOpener}\` but found ${candidates.length}; remediation: confirm the peers_rotate token-persistence comment opener is present exactly once`,
    );
  } else {
    const openerLine = candidates[0];
    let closerLine = openerLine;
    for (let offset = 1; offset <= commentWindowLines; offset += 1) {
      const targetIndex = openerLine - 1 + offset;
      if (targetIndex >= lines.length) {
        break;
      }
      if (!lines[targetIndex].trim().startsWith("//")) {
        break;
      }
      closerLine = targetIndex + 1;
    }
    commentRange = [openerLine, closerLine];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the side-effect comment range cite in the peers rotate section`,
    );
  } else if (commentRange !== null) {
    const cited = [parseInt(match[1], 10), parseInt(match[2], 10)];
    if (cited[0] !== commentRange[0] || cited[1] !== commentRange[1]) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited[0]}-${cited[1]} but the comment block spans :${commentRange[0]}-${commentRange[1]}; remediation: update the citation to :${commentRange[0]}-${commentRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-peers-rotate-side-effect-comment-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-peers-rotate-side-effect-comment-line-refs: ok (comment block main.rs:${commentRange[0]}-${commentRange[1]})`,
);
