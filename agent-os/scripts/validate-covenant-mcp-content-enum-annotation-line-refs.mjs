#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-mcp Content enum annotation line-ref drift guard.
// docs/ipc-and-http-gateway.md line 159 cites the Content tagged-enum
// "defined at `agent-os/crates/covenant-mcp/src/lib.rs:39` with
// `#[serde(tag = "type", rename_all = "camelCase")]`". The line 39
// reference is already pinned by validate-covenant-mcp-struct-line-refs.mjs,
// but the `#[serde(tag = "type", rename_all = "camelCase")]` literal
// next to it is not currently bound to source: a change to the
// discriminator key (e.g. `tag = "kind"`) or the variant casing would
// invalidate the docs prose silently.
//
// This validator asserts:
//   1. The immediately-preceding line above `pub enum Content {` in
//      covenant-mcp/src/lib.rs is exactly the expected annotation
//      literal.
//   2. The docs prose contains the same annotation literal verbatim
//      next to the Content citation.
//
// Convention: same annotation-above-decl shape used by
// validate-covenant-mcp-tool-spec-annotation-line-refs.mjs, except the
// docs cite the enum line (not the annotation line), so the validator
// pins the annotation text rather than a line number.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-mcp/src/lib.rs";

const expectedAnnotation = '#[serde(tag = "type", rename_all = "camelCase")]';
const citation = {
  regex:
    /The variants are defined at `agent-os\/crates\/covenant-mcp\/src\/lib\.rs:\d+` with `#\[serde\(tag = "type", rename_all = "camelCase"\)\]`/,
  label: "Content enum annotation literal citation",
  template:
    "The variants are defined at `agent-os/crates/covenant-mcp/src/lib.rs:NN` with `#[serde(tag = \"type\", rename_all = \"camelCase\")]`",
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

let annotationLine = null;
if (source) {
  const lines = source.split("\n");
  const matches = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+Content\b/.test(lines[index])) {
      matches.push(index + 1);
    }
  }
  if (matches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum Content" at top level but found ${matches.length}; remediation: confirm the Content enum exists at top level in covenant-mcp/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    const enumLine = matches[0];
    if (enumLine < 2) {
      fail(
        `${sourcePath}: "pub enum Content" is at line ${enumLine}; expected the #[serde(tag = ...)] annotation on the preceding line, but the enum is at the top of the file`,
      );
    } else {
      const candidate = enumLine - 1;
      const text = lines[candidate - 1];
      if (text !== expectedAnnotation) {
        fail(
          `${sourcePath}:${candidate}: expected exactly \`${expectedAnnotation}\` immediately above "pub enum Content" at line ${enumLine}, but found \`${text}\`; remediation: restore the annotation on the line directly above the enum declaration, or update both source and docs together if the wire format changed`,
        );
      } else {
        annotationLine = candidate;
      }
    }
  }
}

if (docs) {
  if (!citation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the docs sentence that records the Content enum and the \`${expectedAnnotation}\` literal verbatim`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-mcp-content-enum-annotation-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-mcp-content-enum-annotation-line-refs: ok (Content annotation lib.rs:${annotationLine})`,
);
