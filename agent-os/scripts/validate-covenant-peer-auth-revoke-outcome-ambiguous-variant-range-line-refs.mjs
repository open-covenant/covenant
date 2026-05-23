#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-peer-auth RevokeOutcome::Ambiguous variant range line-ref
// drift guard.
//
// docs/ipc-and-http-gateway.md line 342 cites the Ambiguous variant
// struct-block as a range in agent-os/crates/covenant-peer-auth/src/lib.rs:
//
//   see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:207-211`
//
// The range covers `Ambiguous {` at :207, the two fields (with the
// inner `#[serde(default)]` truncated field at :209-210), and the
// closing `},` at :211. The Ambiguous variant lives inside `pub enum
// RevokeOutcome` whose start line is already covered by
// validate-covenant-peer-auth-struct-line-refs.mjs at :182.
//
// This validator finds the RevokeOutcome enum's brace-balanced span,
// locates the indented `Ambiguous {` line inside, then walks forward
// with brace balance until depth returns to 0 — that's the variant's
// closing brace. The convention is "struct-shaped enum variant block,
// range from variant opener to matching closing brace, scoped to the
// containing enum".

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-peer-auth/src/lib.rs";

const citation = {
  regex:
    /see `RevokeOutcome::Ambiguous` at `covenant-peer-auth\/src\/lib\.rs:(\d+)-(\d+)`/,
  label: "RevokeOutcome::Ambiguous variant range citation",
  template:
    "see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:N-M`",
};

function findBraceBalancedSpan(lines, openerIndex) {
  let depth = 0;
  let opened = false;
  for (let index = openerIndex; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return index;
    }
  }
  return -1;
}

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

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  let enumStart = -1;
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+RevokeOutcome\b/.test(lines[index])) {
      enumStart = index;
      break;
    }
  }
  if (enumStart === -1) {
    fail(
      `${sourcePath}: could not find "pub enum RevokeOutcome" at top level; remediation: confirm the enum exists and is not renamed`,
    );
  } else {
    const enumEnd = findBraceBalancedSpan(lines, enumStart);
    if (enumEnd === -1) {
      fail(
        `${sourcePath}: "pub enum RevokeOutcome" at line ${enumStart + 1} is not brace-balanced; remediation: confirm the enum body is closed`,
      );
    } else {
      let ambiguousStart = -1;
      for (let index = enumStart + 1; index < enumEnd; index += 1) {
        if (/^\s+Ambiguous\s*\{/.test(lines[index])) {
          ambiguousStart = index;
          break;
        }
      }
      if (ambiguousStart === -1) {
        fail(
          `${sourcePath}: could not find "Ambiguous {" struct variant inside RevokeOutcome (lines ${enumStart + 1}-${enumEnd + 1}); remediation: confirm the Ambiguous variant exists as a struct-shaped variant and is not renamed or restructured`,
        );
      } else {
        const ambiguousEnd = findBraceBalancedSpan(lines, ambiguousStart);
        if (ambiguousEnd === -1) {
          fail(
            `${sourcePath}: "Ambiguous {" at line ${ambiguousStart + 1} is not brace-balanced inside RevokeOutcome; remediation: confirm the variant body is closed`,
          );
        } else {
          startLine = ambiguousStart + 1;
          endLine = ambiguousEnd + 1;
        }
      }
    }
  }
}

if (docs) {
  const match = docs.match(citation.regex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the citation that records the RevokeOutcome::Ambiguous variant range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${citation.label} cites covenant-peer-auth/src/lib.rs:${citedStart}-${citedEnd} but the RevokeOutcome::Ambiguous variant spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-peer-auth-revoke-outcome-ambiguous-variant-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-peer-auth-revoke-outcome-ambiguous-variant-range-line-refs: ok (RevokeOutcome::Ambiguous variant lib.rs:${startLine}-${endLine})`,
);
