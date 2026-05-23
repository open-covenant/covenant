#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-a2a A2ATaskResult documented field list drift guard.
// docs/ipc-and-http-gateway.md documents the inner A2ATaskResult
// shape immediately after the A2ATask block (lines ~464-469) with
// a bulleted list pinning four documented field names — `task_id`,
// `status`, `content`, `error_message` — and cites the struct at
// `agent-os/crates/covenant-a2a/src/lib.rs:387`. The :387 struct
// line is already pinned by validate-covenant-a2a-struct-line-refs.mjs,
// and the error_message attribute range is pinned by
// validate-covenant-a2a-field-attribute-range-line-refs.mjs, but the
// four documented field names are not bound to source. A rename or
// removal of any field would silently invalidate the docs prose. The
// field list is part of the public A2A result contract — JSON
// consumers route on `status` to discriminate ok/error/partial
// outcomes, iterate `content` for tagged-enum Content blocks, and
// read `error_message` with key-existence (absent on ok/partial).
//
// This validator asserts:
//   1. Each of the four documented field names exists as a
//      `pub <field>:` declaration inside the brace-balanced
//      A2ATaskResult struct body in covenant-a2a/src/lib.rs.
//   2. The docs prose contains each documented field name as a
//      bullet header (`- \`<field>\` (`type`)`) so the list cannot
//      be quietly paraphrased.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct A2ATaskResult {` opener, so field-name collisions
// across different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-a2a/src/lib.rs";

const expectedFields = ["task_id", "status", "content", "error_message"];

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
    if (/^pub\s+struct\s+A2ATaskResult\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct A2ATaskResult" at top level but found ${openers.length}; remediation: confirm the A2ATaskResult struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct A2ATaskResult" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
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
            `${sourcePath}: expected exactly 1 "pub ${field}:" inside the A2ATaskResult struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${field}\` as a documented A2ATaskResult field; confirm the field exists in source or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  for (const field of expectedFields) {
    const fieldRegex = new RegExp(`^- \`${field}\` \\(`, "m");
    if (!fieldRegex.test(docs)) {
      fail(
        `${docsPath}: missing the A2ATaskResult documented bullet header for field \`${field}\`; remediation: restore the docs bullet that records the type and contract for ${field}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-a2a-task-result-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-a2a-task-result-field-list-line-refs: ok (A2ATaskResult lib.rs:${structStart}-${structEnd}, fields pinned: ${expectedFields.join(", ")})`,
);
