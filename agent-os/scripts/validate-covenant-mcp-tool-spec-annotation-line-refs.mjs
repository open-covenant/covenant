#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-mcp ToolSpec rename_all annotation line-ref drift guard.
// docs/ipc-and-http-gateway.md line 149 cites the
// `#[serde(rename_all = "camelCase")]` annotation that sits immediately
// above `pub struct ToolSpec {` in
// agent-os/crates/covenant-mcp/src/lib.rs:
//
//   `ToolSpec` carries `#[serde(rename_all = "camelCase")]`
//   (`covenant-mcp/src/lib.rs:26`) so the Rust field `input_schema`
//   serializes on the wire as `inputSchema`.
//
// The annotation drives the MCP wire format for the `input_schema` →
// `inputSchema` field rename. The existing covenant-mcp struct
// validator only covers the `pub struct` line at :27 — this validator
// covers the annotation line at :26 using the annotation-above-decl
// convention established by the covenant-types enum annotation
// validator. The docs path uses the shortened
// `covenant-mcp/src/lib.rs:NN` form (relative, no `agent-os/crates/`
// prefix), matching the established annotation citation convention.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-mcp/src/lib.rs";

const expectedAnnotation = '#[serde(rename_all = "camelCase")]';
const citation = {
  regex:
    /`ToolSpec` carries `#\[serde\(rename_all = "camelCase"\)\]` \(`covenant-mcp\/src\/lib\.rs:(\d+)`\)/,
  label: "ToolSpec rename_all annotation citation",
  template:
    "`ToolSpec` carries `#[serde(rename_all = \"camelCase\")]` (`covenant-mcp/src/lib.rs:NN`)",
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
    if (/^pub\s+struct\s+ToolSpec\b/.test(lines[index])) {
      matches.push(index + 1);
    }
  }
  if (matches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub struct ToolSpec" at top level but found ${matches.length}; remediation: confirm the ToolSpec struct exists at top level in covenant-mcp/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    const structLine = matches[0];
    if (structLine < 2) {
      fail(
        `${sourcePath}: "pub struct ToolSpec" is at line ${structLine}; expected the #[serde(rename_all)] annotation on the preceding line, but the struct is at the top of the file`,
      );
    } else {
      const candidate = structLine - 1;
      const text = lines[candidate - 1];
      if (text !== expectedAnnotation) {
        fail(
          `${sourcePath}:${candidate}: expected exactly \`${expectedAnnotation}\` immediately above "pub struct ToolSpec" at line ${structLine}, but found \`${text}\`; remediation: restore the annotation on the line directly above the struct declaration, or update both source and docs together if the wire format changed`,
        );
      } else {
        annotationLine = candidate;
      }
    }
  }
}

if (docs) {
  const match = docs.match(citation.regex);
  if (!match) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the citation that records the ToolSpec annotation line ref`,
    );
  } else if (annotationLine !== null) {
    const cited = parseInt(match[1], 10);
    if (cited !== annotationLine) {
      fail(
        `${docsPath}: the ${citation.label} cites covenant-mcp/src/lib.rs:${cited} but \`${expectedAnnotation}\` for "pub struct ToolSpec" is at line ${annotationLine}; remediation: update the citation to :${annotationLine}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-mcp-tool-spec-annotation-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-mcp-tool-spec-annotation-line-refs: ok (ToolSpec annotation lib.rs:${annotationLine})`,
);
