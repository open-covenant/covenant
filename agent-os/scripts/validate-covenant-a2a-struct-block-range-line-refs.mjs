#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a struct block range line-ref drift guard.
// docs/ipc-and-http-gateway.md line 462 cites the A2AIdempotency
// struct block in agent-os/crates/covenant-a2a/src/lib.rs as a range
// citation:
//
//   `(defined at `covenant-a2a/src/lib.rs:55-59`)`
//
// The range spans the contiguous `#[...]` attribute block (typically
// `#[derive(...)]`), the `pub struct A2AIdempotency {` opener, the
// field lines, and the matching closing brace.
//
// The existing covenant-a2a validators do not cover this convention:
// the struct line-ref validator only matches single-line `pub struct`
// citations; the field-attribute-range validator's ranges terminate
// at a `pub <field>:` declaration rather than a closing brace; and
// the enum-block-range validator only matches `pub enum`.
//
// This validator finds the `pub struct A2AIdempotency` line, walks
// backwards while the previous line starts with `#[` to find the
// range start, then walks forward from the struct opener with a
// brace-balance scan until depth returns to 0 to find the range end.
// Line numbers are never hardcoded.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const target = {
  name: "A2AIdempotency",
  docsRegex:
    /optional `A2AIdempotency` `\{duplicate_safety: "unsafe"\|"idempotent", key: string\}` \(defined at `covenant-a2a\/src\/lib\.rs:(\d+)-(\d+)`\)/,
  docsLabel: "A2AIdempotency struct block range citation",
  docsTemplate:
    "optional `A2AIdempotency` `{duplicate_safety: \"unsafe\"|\"idempotent\", key: string}` (defined at `covenant-a2a/src/lib.rs:N-M`)",
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
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (new RegExp(`^pub\\s+struct\\s+${target.name}\\b`).test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct ${target.name}" at top level but found ${openers.length}; remediation: confirm the ${target.name} struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    const structLine = openers[0];
    startLine = structLine;
    for (let index = structLine - 2; index >= 0; index -= 1) {
      const trimmed = lines[index].trim();
      if (trimmed.startsWith("#[")) {
        startLine = index + 1;
      } else {
        break;
      }
    }
    if (startLine === structLine) {
      fail(
        `${sourcePath}:${structLine}: expected one or more "#[...]" attribute lines immediately above "pub struct ${target.name}", but the preceding line is not an attribute; remediation: restore the attribute block above the struct declaration`,
      );
    } else {
      let depth = 0;
      let opened = false;
      for (let index = structLine - 1; index < lines.length; index += 1) {
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
          `${sourcePath}: could not find the matching closing brace for "pub struct ${target.name}" starting at line ${structLine}; remediation: confirm the struct body is brace-balanced`,
        );
      }
    }
  }
}

if (docs) {
  const match = docs.match(target.docsRegex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${target.docsLabel} ("${target.docsTemplate}"); remediation: restore the citation that records the ${target.name} struct block range`,
    );
  } else if (startLine !== null && endLine !== null) {
    const citedStart = parseInt(match[1], 10);
    const citedEnd = parseInt(match[2], 10);
    if (citedStart !== startLine || citedEnd !== endLine) {
      fail(
        `${docsPath}: the ${target.docsLabel} cites covenant-a2a/src/lib.rs:${citedStart}-${citedEnd} but the ${target.name} struct block spans :${startLine}-${endLine}; remediation: update the citation to :${startLine}-${endLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-struct-block-range-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-a2a-struct-block-range-line-refs: ok (${target.name} block lib.rs:${startLine}-${endLine})`,
);
