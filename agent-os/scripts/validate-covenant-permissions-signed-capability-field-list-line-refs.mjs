#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-permissions SignedCapability documented field list drift guard.
// docs/ipc-and-http-gateway.md (line 249) documents the SignedCapability
// shape inline as `{capability: Capability, signature: <base58>}` and
// cites the struct at `agent-os/crates/covenant-permissions/src/lib.rs:58`.
// The :58 struct line is pinned by
// validate-covenant-permissions-struct-line-refs.mjs and the sig_b58
// module range (:64-84) is pinned by
// validate-covenant-permissions-sig-b58-module-range-line-refs.mjs,
// but the two documented field names `capability` and `signature` are
// not bound to source. A Rust-side rename of either field — or a
// third pub field silently added to the struct — would invalidate the
// documented shape without tripping any existing anchor.
//
// This validator asserts:
//   1. The brace-balanced SignedCapability struct body in
//      covenant-permissions/src/lib.rs contains exactly two
//      `pub <field>:` declarations, named `capability` and `signature`.
//   2. The docs prose contains the inline shape token
//      `{capability: Capability, signature: <base58>}` so the documented
//      field list cannot be quietly paraphrased.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct SignedCapability {` opener, so field-name collisions
// across different structs do not contaminate the lookup. An exact
// field-count assertion catches added-field drift, which the existing
// struct-line anchor cannot see.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-permissions/src/lib.rs";

const expectedFields = ["capability", "signature"];

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
let actualFields = null;
if (source) {
  const lines = source.split("\n");
  const openers = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+struct\s+SignedCapability\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct SignedCapability" at top level but found ${openers.length}; remediation: confirm the SignedCapability struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct SignedCapability" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
      );
    } else {
      const fieldRegex = /^\s*pub\s+(\w+)\s*:/;
      actualFields = [];
      for (let index = structStart; index < structEnd; index += 1) {
        const match = lines[index].match(fieldRegex);
        if (match) {
          actualFields.push(match[1]);
        }
      }
      if (
        actualFields.length !== expectedFields.length ||
        !expectedFields.every((field, position) => actualFields[position] === field)
      ) {
        fail(
          `${sourcePath}: expected SignedCapability struct body (lines ${structStart}-${structEnd}) to declare exactly [${expectedFields.join(", ")}] but found [${actualFields.join(", ")}]; remediation: the docs document SignedCapability as the two-field shape \`{capability: Capability, signature: <base58>}\`; update both docs and source together if the field list changes`,
        );
      }
    }
  }
}

if (docs) {
  const shapeToken = "shape `{capability: Capability, signature: <base58>}`";
  const occurrences = docs.split(shapeToken).length - 1;
  if (occurrences !== 1) {
    fail(
      `${docsPath}: expected exactly 1 occurrence of the SignedCapability inline shape token "${shapeToken}" but found ${occurrences}; remediation: restore the documented shape so consumers see the two field names \`capability\` and \`signature\` verbatim, not a paraphrase`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-permissions-signed-capability-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-permissions-signed-capability-field-list-line-refs: ok (SignedCapability lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
