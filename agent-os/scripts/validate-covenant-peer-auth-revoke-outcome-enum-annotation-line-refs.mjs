#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-peer-auth RevokeOutcome enum annotation line-ref drift guard.
// docs/ipc-and-http-gateway.md line 335 cites the RevokeOutcome
// tagged-enum "defined at
// `agent-os/crates/covenant-peer-auth/src/lib.rs:182` with
// `#[serde(tag = "type", rename_all = "snake_case")]`". The line 182
// reference is already pinned by
// validate-covenant-peer-auth-struct-line-refs.mjs, but the
// `#[serde(tag = "type", rename_all = "snake_case")]` literal next to
// it is not currently bound to source: a change to the discriminator
// key (e.g. `tag = "kind"`) or the variant casing would invalidate
// the docs prose silently.
//
// This validator asserts:
//   1. The immediately-preceding line above `pub enum RevokeOutcome {`
//      in covenant-peer-auth/src/lib.rs is exactly the expected
//      annotation literal.
//   2. The docs prose contains the same annotation literal verbatim
//      next to the RevokeOutcome citation.
//
// Convention: same shape used by
// validate-covenant-mcp-content-enum-annotation-line-refs.mjs.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-peer-auth/src/lib.rs";

const expectedAnnotation = '#[serde(tag = "type", rename_all = "snake_case")]';
const citation = {
  regex:
    /a tagged-enum `RevokeOutcome` \(defined at `agent-os\/crates\/covenant-peer-auth\/src\/lib\.rs:\d+` with `#\[serde\(tag = "type", rename_all = "snake_case"\)\]`\)/,
  label: "RevokeOutcome enum annotation literal citation",
  template:
    "a tagged-enum `RevokeOutcome` (defined at `agent-os/crates/covenant-peer-auth/src/lib.rs:NN` with `#[serde(tag = \"type\", rename_all = \"snake_case\")]`)",
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
    if (/^pub\s+enum\s+RevokeOutcome\b/.test(lines[index])) {
      matches.push(index + 1);
    }
  }
  if (matches.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum RevokeOutcome" at top level but found ${matches.length}; remediation: confirm the RevokeOutcome enum exists at top level in covenant-peer-auth/src/lib.rs and is not renamed, moved, or duplicated`,
    );
  } else {
    const enumLine = matches[0];
    if (enumLine < 2) {
      fail(
        `${sourcePath}: "pub enum RevokeOutcome" is at line ${enumLine}; expected the #[serde(tag = ...)] annotation on the preceding line, but the enum is at the top of the file`,
      );
    } else {
      const candidate = enumLine - 1;
      const text = lines[candidate - 1];
      if (text !== expectedAnnotation) {
        fail(
          `${sourcePath}:${candidate}: expected exactly \`${expectedAnnotation}\` immediately above "pub enum RevokeOutcome" at line ${enumLine}, but found \`${text}\`; remediation: restore the annotation on the line directly above the enum declaration, or update both source and docs together if the wire format changed`,
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
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the docs sentence that records the RevokeOutcome enum and the \`${expectedAnnotation}\` literal verbatim`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-peer-auth-revoke-outcome-enum-annotation-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-peer-auth-revoke-outcome-enum-annotation-line-refs: ok (RevokeOutcome annotation lib.rs:${annotationLine})`,
);
