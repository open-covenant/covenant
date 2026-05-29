#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-mcp ToolSpec documented field list drift guard.
// docs/ipc-and-http-gateway.md documents the ToolSpec shape
// immediately after the tool_list envelope (lines ~143-147) with a
// bulleted list pinning three documented field names — `name`,
// `description`, `inputSchema` — and cites the struct at
// `agent-os/crates/covenant-mcp/src/lib.rs:27`.
// The :27 struct line is pinned by
// validate-covenant-mcp-struct-line-refs.mjs and the rename_all
// annotation is pinned by
// validate-covenant-mcp-tool-spec-annotation-line-refs.mjs, but the
// three documented field names are not bound to source.
//
// The wire-form `inputSchema` maps to source-form `input_schema`
// because of `#[serde(rename_all = "camelCase")]` on the struct.
// The validator applies the docs→source mapping below so the bullet
// header check uses the camelCase wire form (what JSON consumers
// see) and the source-side `pub` check uses the snake_case Rust form
// (what the struct actually declares).
//
// This validator asserts:
//   1. Each docs-form bullet header (`- \`<wire>\` (`type`)`) is
//      present in the docs prose.
//   2. Each source-form field name appears as exactly one
//      `pub <field>:` declaration inside the brace-balanced
//      ToolSpec struct body in covenant-mcp/src/lib.rs.
//
// Convention: the struct body is located by brace-balance from the
// `pub struct ToolSpec {` opener, so field-name collisions across
// different structs do not contaminate the lookup.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-mcp/src/lib.rs";

const docsToSource = {
  name: "name",
  description: "description",
  inputSchema: "input_schema",
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
    if (/^pub\s+struct\s+ToolSpec\b/.test(lines[index])) {
      openers.push(index + 1);
    }
  }
  if (openers.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct ToolSpec" at top level but found ${openers.length}; remediation: confirm the ToolSpec struct exists at top level and is not renamed, moved, or duplicated`,
    );
  } else {
    structStart = openers[0];
    structEnd = scanBraceBalance(lines, structStart);
    if (structEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub struct ToolSpec" starting at line ${structStart}; remediation: confirm the struct body is brace-balanced`,
      );
    } else {
      for (const [docsField, sourceField] of Object.entries(docsToSource)) {
        const fieldRegex = new RegExp(`^\\s*pub\\s+${sourceField}\\s*:`);
        const matches = [];
        for (let index = structStart; index < structEnd; index += 1) {
          if (fieldRegex.test(lines[index])) {
            matches.push(index + 1);
          }
        }
        if (matches.length !== 1) {
          fail(
            `${sourcePath}: expected exactly 1 "pub ${sourceField}:" inside the ToolSpec struct (lines ${structStart}-${structEnd}) but found ${matches.length}; remediation: the docs cite \`${docsField}\` (wire form) which maps to \`${sourceField}\` (source form) via #[serde(rename_all = "camelCase")]; confirm the field exists in source or update both docs and source together`,
          );
        }
      }
    }
  }
}

if (docs) {
  for (const docsField of Object.keys(docsToSource)) {
    const fieldRegex = new RegExp(`^- \`${docsField}\` \\(`, "m");
    if (!fieldRegex.test(docs)) {
      fail(
        `${docsPath}: missing the ToolSpec documented bullet header for field \`${docsField}\`; remediation: restore the docs bullet that records the type and contract for ${docsField}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-mcp-tool-spec-field-list-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

const pairs = Object.entries(docsToSource)
  .map(([docsField, sourceField]) =>
    docsField === sourceField ? docsField : `${docsField}->${sourceField}`,
  )
  .join(", ");
console.log(
  `validate-covenant-mcp-tool-spec-field-list-line-refs: ok (ToolSpec lib.rs:${structStart}-${structEnd}, fields pinned: ${pairs})`,
);
