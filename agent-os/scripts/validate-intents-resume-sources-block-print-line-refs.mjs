#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// intents_resume sources-block CLI print line-ref drift guard.
// docs/ipc-and-http-gateway.md line 614 (the intents_resume `sources`
// field bullet) cites main.rs:3941-3944 for the unsuffixed CLI block
// that prints a `sources:` header followed by indented `  - <label>`
// lines, only when the array is non-empty.
//
// L614 also carries the 5385-5388 `Pinned as an array of strings` type
// cite, which the intents-resume type-level validator already guards.
// This validator binds the SEPARATE print-block cite: it anchors on the
// unique `println!("sources:");` opener, asserts the following
// `for s in sources {` loop opener, and derives the loop's closer at the
// opener indentation (`}`) — the inner per-label println at deeper indent
// cannot mis-close the range.
//
// docsRegex anchors on the L614-unique `sources:` block followed by ...
// phrase, which trails the type-pin cite, so first-match capture binds
// the print-block range rather than the type pin.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const printOpener = 'println!("sources:");';
const loopOpener = "for s in sources {";
const closerWindowLines = 12;

const docsRegex =
  /`sources:` block followed by `  - <label>` lines at `main\.rs:(\d+)-(\d+)`/;
const docsLabel = "intents_resume sources-block print range citation";
const docsTemplate =
  "`sources:` block followed by `  - <label>` lines at `main.rs:N-M`";

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

let blockRange = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (lines[index].trim() === printOpener) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${printOpener}\` but found ${openers.length}; remediation: confirm the intents_resume CLI prints the sources header exactly once`,
    );
  } else {
    const openerLine = openers[0];
    const loopLineText = lines[openerLine] ?? "";
    if (loopLineText.trim() !== loopOpener) {
      fail(
        `${sourcePath}:${openerLine + 1}: expected \`${loopOpener}\` immediately after the sources header print but found \`${loopLineText.trim()}\`; remediation: confirm the per-label loop directly follows the sources: print`,
      );
    } else {
      const indent = loopLineText.match(/^\s*/)[0];
      const closer = `${indent}}`;
      let closerLine = null;
      for (let offset = 1; offset <= closerWindowLines; offset += 1) {
        const targetIndex = openerLine + offset;
        if (targetIndex >= lines.length) {
          break;
        }
        if (lines[targetIndex] === closer) {
          closerLine = targetIndex + 1;
          break;
        }
      }
      if (closerLine === null) {
        fail(
          `${sourcePath}:${openerLine + 1}: no \`}\` at the loop indentation within ${closerWindowLines} lines after the sources loop opener; remediation: confirm the per-label loop is closed at its own indentation`,
        );
      } else {
        blockRange = [openerLine, closerLine];
      }
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the sources print-block range cite in the intents_resume sources bullet`,
    );
  } else if (blockRange !== null) {
    const cited = [parseInt(match[1], 10), parseInt(match[2], 10)];
    if (cited[0] !== blockRange[0] || cited[1] !== blockRange[1]) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited[0]}-${cited[1]} but the sources print block spans :${blockRange[0]}-${blockRange[1]}; remediation: update the citation to :${blockRange[0]}-${blockRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error(
    "validate-intents-resume-sources-block-print-line-refs: failed",
  );
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-intents-resume-sources-block-print-line-refs: ok (sources print block main.rs:${blockRange[0]}-${blockRange[1]})`,
);
