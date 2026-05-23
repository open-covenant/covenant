#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types Capability documented field list drift guard.
// docs/ipc-and-http-gateway.md line 249 cites Capability "defined at
// `agent-os/crates/covenant-types/src/lib.rs:171` (fields: `subject`,
// `action`, `scope`, `granted_by`, `expires_at`)". The :171 struct-line
// reference is already pinned by
// validate-covenant-types-struct-line-refs.mjs, but the five
// documented field names are not bound to source: a rename or removal
// of any of these fields would silently invalidate the docs prose.
// The field list is part of the public capability contract —
// consumers route on these field names.
//
// This validator asserts:
//   1. Each of the five documented field names exists as a
//      `pub <field>:` declaration inside the brace-balanced Capability
//      struct body in covenant-types/src/lib.rs.
//   2. The docs prose contains each field name in backticks within
//      the Capability-field-list citation.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct Capability {` opener, so field-name collisions across
// different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const expectedFields = ["subject", "action", "scope", "granted_by", "expires_at"];

const docsCitation = {
  regex:
    /`Capability` is defined at `agent-os\/crates\/covenant-types\/src\/lib\.rs:\d+` \(fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`\)/,
  label: "Capability field list citation",
  template:
    "`Capability` is defined at `agent-os/crates/covenant-types/src/lib.rs:NN` (fields: `subject`, `action`, `scope`, `granted_by`, `expires_at`)",
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
    if (/^pub\s+struct\s+Capability\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct Capability" at top level but found ${openers.length}; remediation: confirm the Capability struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct Capability" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "pub ${field}:" inside the Capability struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${field}\` as a documented Capability field; confirm the field exists in source or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  if (!docsCitation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${docsCitation.label} ("${docsCitation.template}"); remediation: restore the docs sentence that records all five field names in the documented order — subject, action, scope, granted_by, expires_at`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-capability-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-capability-field-list-line-refs: ok (Capability lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
