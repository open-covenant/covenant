#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-mcp struct/enum line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two name-anchored declarations in
// agent-os/crates/covenant-mcp/src/lib.rs:
//
//   - The inner `ToolSpec` shape, defined at
//     `agent-os/crates/covenant-mcp/src/lib.rs:NN`:
//   - The variants are defined at
//     `agent-os/crates/covenant-mcp/src/lib.rs:NN` with
//     `#[serde(tag = "type", rename_all = "camelCase")]`
//
// The second sentence describes the `Content` tagged-enum and follows
// the same convention established by covenant-peer-auth's RevokeOutcome
// citation: the enum declaration line is the anchor, not the
// `#[serde(...)]` annotation above it.
//
// Both line numbers shift whenever covenant-mcp/src/lib.rs grows above
// the cited declarations, and the docs do not auto-update.
//
// This validator derives the line numbers from
// covenant-mcp/src/lib.rs at run time by matching `pub struct ToolSpec`
// and `pub enum Content` with strict top-level regexes (no indentation).

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const structsPath = "agent-os/crates/covenant-mcp/src/lib.rs";

const targets = {
  ToolSpec: {
    kind: "struct",
    matches: [],
    line: null,
    docsRegex: /The inner `ToolSpec` shape, defined at `agent-os\/crates\/covenant-mcp\/src\/lib\.rs:(\d+)`:/,
    docsLabel: "ToolSpec citation",
    docsTemplate: "The inner `ToolSpec` shape, defined at `agent-os/crates/covenant-mcp/src/lib.rs:NN`:",
  },
  Content: {
    kind: "enum",
    matches: [],
    line: null,
    docsRegex: /The variants are defined at `agent-os\/crates\/covenant-mcp\/src\/lib\.rs:(\d+)` with `#\[serde\(tag = "type", rename_all = "camelCase"\)\]`/,
    docsLabel: "Content citation",
    docsTemplate: "The variants are defined at `agent-os/crates/covenant-mcp/src/lib.rs:NN` with `#[serde(tag = \"type\", rename_all = \"camelCase\")]`",
  },
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
  source = read(structsPath);
} catch (error) {
  fail(`cannot read ${structsPath}: ${error.message}`);
}

if (source) {
  const lines = source.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(/^pub\s+(struct|enum)\s+(\w+)\b/);
    if (match && targets[match[2]] !== undefined && targets[match[2]].kind === match[1]) {
      targets[match[2]].matches.push(index + 1);
    }
  }
  for (const [name, entry] of Object.entries(targets)) {
    if (entry.matches.length !== 1) {
      fail(
        `${structsPath}: expected exactly 1 "pub ${entry.kind} ${name}" but found ${entry.matches.length}; remediation: confirm the ${name} ${entry.kind} exists at top level in covenant-mcp/src/lib.rs and is not renamed, moved, or duplicated`,
      );
    } else {
      entry.line = entry.matches[0];
    }
  }
}

if (docs) {
  for (const [name, entry] of Object.entries(targets)) {
    const match = docs.match(entry.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${entry.docsLabel} ("${entry.docsTemplate}"); remediation: restore the citation that records the ${name} ${entry.kind} line ref`,
      );
      continue;
    }
    if (entry.line !== null) {
      const cited = parseInt(match[1], 10);
      if (cited !== entry.line) {
        fail(
          `${docsPath}: the ${entry.docsLabel} cites covenant-mcp/src/lib.rs:${cited} but pub ${entry.kind} ${name} is at line ${entry.line}; remediation: update the citation to :${entry.line}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-mcp-struct-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-mcp-struct-line-refs: ok (ToolSpec lib.rs:${targets.ToolSpec.line}, Content lib.rs:${targets.Content.line})`,
);
