#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-permissions sig_b58 module range line-ref drift guard.
// docs/ipc-and-http-gateway.md cites the sig_b58 serde module in
// agent-os/crates/covenant-permissions/src/lib.rs as a relative-form
// range citation: the SignedCapability sentence at docs line 249 names
// the absolute path once, then uses the shortened `lib.rs:64-83` form
// for the sig_b58 module range.
//
// The range covers the `mod sig_b58 {` opener through its matching
// closing brace. The existing validate-covenant-permissions-struct-line-refs.mjs
// covers only the SignedCapability struct line and explicitly excludes
// this range citation because the anchor convention is different
// (module declaration plus brace-balanced span instead of a single
// `pub struct` line).
//
// This validator derives the module start line by matching
// `mod sig_b58 {` at top level, then walks forward incrementing on `{`
// and decrementing on `}` until balance returns to 0; that line is the
// end. Both endpoints must match the cited range.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-permissions/src/lib.rs";

const citation = {
  regex: /per the `sig_b58` serde module at `lib\.rs:(\d+)-(\d+)`/,
  label: "sig_b58 module range citation",
  template: "per the `sig_b58` serde module at `lib.rs:N-M`",
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

let startLine = null;
let endLine = null;
if (source) {
  const lines = source.split("\n");
  const starts = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^mod\s+sig_b58\s*\{/.test(lines[index])) {
      starts.push(index + 1);
    }
  }
  if (starts.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "mod sig_b58 {" at top level but found ${starts.length}; remediation: confirm the sig_b58 serde module exists at top level in covenant-permissions/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    startLine = starts[0];
    let depth = 0;
    let opened = false;
    for (let index = startLine - 1; index < lines.length; index += 1) {
      for (const char of lines[index]) {
        if (char === "{") {
          depth += 1;
          opened = true;
        } else if (char === "}") {
          depth -= 1;
        }
      }
      if (opened && depth === 0) {
        endLine = index + 1;
        break;
      }
    }
    if (endLine === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "mod sig_b58 {" starting at line ${startLine}; remediation: confirm the module body is brace-balanced`,
      );
    }
  }
}

if (docs) {
  const match = docs.match(citation.regex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the citation that records the sig_b58 module range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${citation.label} cites lib.rs:${citedStart}-${citedEnd} but "mod sig_b58 { ... }" spans lines ${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-permissions-sig-b58-module-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-permissions-sig-b58-module-range-line-refs: ok (mod sig_b58 lib.rs:${startLine}-${endLine})`,
);
