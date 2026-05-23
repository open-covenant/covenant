#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types SettlementReceipt tx_sig doc-comment paragraph range
// line-ref drift guard.
//
// docs/ipc-and-http-gateway.md line 217 cites the backwards-compat
// paragraph in the SettlementReceipt struct's doc-comment as a range
// citation in agent-os/crates/covenant-types/src/lib.rs:335-337:
//
//   `onchain_sig` (string or null) — backwards-compatible alias for
//   `tx_sig` (per the struct doc-comment at
//   `covenant-types/src/lib.rs:335-337`) ...
//
// The cited paragraph documents the onchain_sig → tx_sig migration and
// sits above `pub struct SettlementReceipt` (currently :339), separated
// from an earlier first-paragraph doc-comment line at :333 by a blank
// `///` line at :334. The convention is "doc-comment paragraph above
// the struct that contains the unique anchor phrase
// `backwards-compatible alias for`, bounded above and below by either
// a blank `///` paragraph separator or by the start/end of the
// contiguous doc-comment block".
//
// This validator finds `pub struct SettlementReceipt`, walks back
// skipping `#[...]` attribute lines, collects the contiguous `///`
// doc-comment block above, locates the paragraph containing the anchor
// phrase, and derives the paragraph boundaries via blank `///` lines.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const anchorPhrase = "backwards-compatible alias for";
const citation = {
  regex:
    /per the struct doc-comment at `covenant-types\/src\/lib\.rs:(\d+)-(\d+)`/,
  label: "SettlementReceipt tx_sig doc-comment range citation",
  template:
    "per the struct doc-comment at `covenant-types/src/lib.rs:N-M`",
};

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

const isDocComment = (line) => /^\s*\/\/\//.test(line);
const isBlankDocComment = (line) => /^\s*\/\/\/\s*$/.test(line);
const isAttribute = (line) => /^\s*#\[/.test(line);

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  let structIndex = -1;
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+struct\s+SettlementReceipt\b/.test(lines[index])) {
      structIndex = index;
      break;
    }
  }
  if (structIndex === -1) {
    fail(
      `${sourcePath}: could not find "pub struct SettlementReceipt" at top level; remediation: confirm the struct exists and is not renamed`,
    );
  } else {
    let cursor = structIndex - 1;
    while (cursor >= 0 && isAttribute(lines[cursor])) {
      cursor -= 1;
    }
    const docBlockEnd = cursor;
    while (cursor >= 0 && isDocComment(lines[cursor])) {
      cursor -= 1;
    }
    const docBlockStart = cursor + 1;
    if (docBlockStart > docBlockEnd) {
      fail(
        `${sourcePath}: expected a /// doc-comment block above the SettlementReceipt struct (after stripping any #[...] attributes), but found none; remediation: restore the doc-comment block that documents the backwards-compat alias`,
      );
    } else {
      let anchorIndex = -1;
      for (let index = docBlockStart; index <= docBlockEnd; index += 1) {
        if (lines[index].includes(anchorPhrase)) {
          anchorIndex = index;
          break;
        }
      }
      if (anchorIndex === -1) {
        fail(
          `${sourcePath}: the doc-comment block above SettlementReceipt (lines ${docBlockStart + 1}-${docBlockEnd + 1}) does not contain the anchor phrase \`${anchorPhrase}\`; remediation: restore the backwards-compat paragraph or update the validator if the wording changed`,
        );
      } else {
        let paragraphStart = anchorIndex;
        while (
          paragraphStart > docBlockStart &&
          !isBlankDocComment(lines[paragraphStart - 1])
        ) {
          paragraphStart -= 1;
        }
        let paragraphEnd = anchorIndex;
        while (
          paragraphEnd < docBlockEnd &&
          !isBlankDocComment(lines[paragraphEnd + 1])
        ) {
          paragraphEnd += 1;
        }
        startLine = paragraphStart + 1;
        endLine = paragraphEnd + 1;
      }
    }
  }
}

if (docs) {
  const match = docs.match(citation.regex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the citation that records the SettlementReceipt tx_sig doc-comment range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${citation.label} cites covenant-types/src/lib.rs:${citedStart}-${citedEnd} but the backwards-compat paragraph in the SettlementReceipt doc-comment spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-tx-sig-doc-comment-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-tx-sig-doc-comment-range-line-refs: ok (SettlementReceipt backwards-compat paragraph lib.rs:${startLine}-${endLine})`,
);
