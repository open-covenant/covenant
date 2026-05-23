#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a A2AIdempotency documented field list drift guard.
// docs/ipc-and-http-gateway.md line 462 cites the A2AIdempotency
// struct as ``A2AIdempotency` `{duplicate_safety: "unsafe"|"idempotent",
// key: string}` (defined at `covenant-a2a/src/lib.rs:55-59`)`. The
// :55-59 struct block range is already pinned by
// validate-covenant-a2a-struct-block-range-line-refs.mjs, but the two
// documented field names (`duplicate_safety`, `key`) are not bound to
// source: a rename or removal of either field would silently invalidate
// the docs prose while the line range still matches. The field list is
// part of the public idempotency contract — JSON consumers route on
// these field names.
//
// This validator asserts:
//   1. Each of the two documented field names exists as a
//      `pub <field>:` declaration inside the brace-balanced
//      A2AIdempotency struct body in covenant-a2a/src/lib.rs.
//   2. The docs prose contains the documented field-list citation
//      verbatim (regex match) so the field names cannot be quietly
//      paraphrased into a different shape.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct A2AIdempotency {` opener, so field-name collisions across
// different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const expectedFields = ["duplicate_safety", "key"];

const docsCitation = {
  regex:
    /`A2AIdempotency` `\{duplicate_safety: "unsafe"\|"idempotent", key: string\}` \(defined at `covenant-a2a\/src\/lib\.rs:\d+-\d+`\)/,
  label: "A2AIdempotency documented field list citation",
  template:
    "`A2AIdempotency` `{duplicate_safety: \"unsafe\"|\"idempotent\", key: string}` (defined at `covenant-a2a/src/lib.rs:N-M`)",
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

function scanBraceBalance(lines, openerLine) {
  let depth = 0;
  let opened = false;
  for (let index = openerLine - 1; index < lines.length; index += 1) {
    for (const char of lines[index]) {
      if (char === "{") {
        depth += 1;
        opened = true;
      } else if (char === "}") {
        depth -= 1;
      }
    }
    if (opened && depth === 0) {
      return index + 1;
    }
  }
  return null;
}

let structStart = null;
let structEnd = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+struct\s+A2AIdempotency\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct A2AIdempotency" at top level but found ${openers.length}; remediation: confirm the A2AIdempotency struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct A2AIdempotency" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
      );
    } else {
      for (const field of expectedFields) {
        const fieldRegex = new RegExp(`^\\s*pub\\s+${field}\\s*:`);
        const matches = [];
        for (let index = structStart; index < structEnd; index += 1) {
          if (fieldRegex.test(lines[index])) {
            matches.push(index + 1);
          }
        }
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "pub ${field}:" inside the A2AIdempotency struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${field}\` as a documented A2AIdempotency field; confirm the field exists in source or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records both field names in the documented order — duplicate_safety, key`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-a2a-idempotency-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-a2a-a2a-idempotency-field-list-line-refs: ok (A2AIdempotency lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
