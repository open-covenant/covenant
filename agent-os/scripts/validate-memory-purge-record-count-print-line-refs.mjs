#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// memory purge record-count CLI print line-ref drift guard.
// docs/ipc-and-http-gateway.md line 403 documents the memory purge
// `purged` field and cites main.rs:2164 for the unsuffixed CLI line
// `println!("purged {purged} record(s)");`, confirming the unit is a
// memory record. The aggregate validate-memory-purge-type-level-pin-
// line-refs.mjs matches L403 but with a non-capturing `\d+` for this
// inline cite, asserting only the trailing type pin.
//
// This validator binds the cite directly via the unique println anchor.
// docsRegex anchors on `purged <n> record(s)` at `main.rs:N`; the noun
// `record(s)` distinguishes it from the sibling capabilities/peers/audit
// purge prints (`revoked peer(s)`, `event(s)`), so first-match capture
// cannot drift to another purge sentence.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const printAnchor = 'println!("purged {purged} record(s)");';

const docsRegex = /prints `purged <n> record\(s\)` at `main\.rs:(\d+)`/;
const docsLabel = "memory purge record-count CLI print citation";
const docsTemplate = "prints `purged <n> record(s)` at `main.rs:N`";

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

let printLine = null;
if (source) {
  const lines = source.split("\n");
  const candidates = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === printAnchor) {
      candidates.push(index + 1);
    }
  }
  if (candidates.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${printAnchor}\` but found ${candidates.length}; remediation: confirm the memory purge CLI prints the record count exactly once`,
    );
  } else {
    printLine = candidates[0];
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the record-count print cite in the memory purge purged bullet`,
    );
  } else if (printLine !== null) {
    const cited = parseInt(match[1], 10);
    if (cited !== printLine) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited} but the println lives at :${printLine}; remediation: update the citation to :${printLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-memory-purge-record-count-print-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-memory-purge-record-count-print-line-refs: ok (println main.rs:${printLine})`,
);
