#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// receipt (local-only) onchain-fallback line-ref drift guard.
// docs/ipc-and-http-gateway.md line 217 documents the `onchain_sig`
// backwards-compatible alias and cites the unsuffixed CLI's
// `(local-only)` fallback block at main.rs:2864-2867 — the
// `match r.tx_sig.as_ref().or(r.onchain_sig.as_ref())` that reads
// `tx_sig` first and falls back to `onchain_sig`, printing
// `(local-only)` when both are absent.
//
// No prior validator bound this cite: the settlement-receipt field-list
// validator guards the SettlementReceipt struct definition, not this
// inline CLI fallback block.
//
// The block opener is unique; its closer is matched at the opener
// indentation (`};`) so the inner `Some`/`None` match arms cannot
// mis-close the range. docsRegex anchors on the L217-unique
// `(local-only)` fallback at `main.rs:N-M` phrase.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant/src/main.rs";

const blockOpener =
  "let onchain = match r.tx_sig.as_ref().or(r.onchain_sig.as_ref()) {";
const closerWindowLines = 20;

const docsRegex = /`\(local-only\)` fallback at `main\.rs:(\d+)-(\d+)`/;
const docsLabel = "receipt (local-only) onchain-fallback range citation";
const docsTemplate = "`(local-only)` fallback at `main.rs:N-M`";

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
    if (lines[index].trim() === blockOpener) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 occurrence of \`${blockOpener}\` but found ${openers.length}; remediation: confirm the receipt onchain/tx_sig fallback match is present exactly once`,
    );
  } else {
    const openerLine = openers[0];
    const indent = lines[openerLine - 1].match(/^\s*/)[0];
    const closer = `${indent}};`;
    let closerLine = null;
    for (let offset = 1; offset <= closerWindowLines; offset += 1) {
      const targetIndex = openerLine - 1 + offset;
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
        `${sourcePath}:${openerLine}: no \`};\` at the opener indentation within ${closerWindowLines} lines after the fallback opener; remediation: confirm the fallback match is closed at its own indentation`,
      );
    } else {
      blockRange = [openerLine, closerLine];
    }
  }
}

if (docs) {
  const match = docs.match(docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${docsLabel} ("${docsTemplate}"); remediation: restore the (local-only) fallback range cite in the onchain_sig alias bullet`,
    );
  } else if (blockRange !== null) {
    const cited = [parseInt(match[1], 10), parseInt(match[2], 10)];
    if (cited[0] !== blockRange[0] || cited[1] !== blockRange[1]) {
      fail(
        `${docsPath}: the ${docsLabel} cites main.rs:${cited[0]}-${cited[1]} but the fallback block spans :${blockRange[0]}-${blockRange[1]}; remediation: update the citation to :${blockRange[0]}-${blockRange[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-receipt-onchain-fallback-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-receipt-onchain-fallback-line-refs: ok (fallback block main.rs:${blockRange[0]}-${blockRange[1]})`,
);
