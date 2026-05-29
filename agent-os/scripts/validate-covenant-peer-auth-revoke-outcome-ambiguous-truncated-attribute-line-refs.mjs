#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-peer-auth RevokeOutcome::Ambiguous truncated field
// #[serde(default)] attribute drift guard.
//
// docs/ipc-and-http-gateway.md line 342 mentions the
// `#[serde(default)]` annotation on the `truncated` field inside the
// `RevokeOutcome::Ambiguous` variant:
//
//   `(see RevokeOutcome::Ambiguous at covenant-peer-auth/src/lib.rs:207-211).
//    The field carries #[serde(default)] so a stale CLI built before
//    truncated landed still deserialises a new daemon's response ...`
//
// The variant range (:207-211) is already pinned by
// validate-covenant-peer-auth-revoke-outcome-ambiguous-variant-range-line-refs.mjs,
// but the `#[serde(default)]` attribute literal next to `truncated` is
// not currently bound to source: a removal of that annotation would
// silently break the backwards-compat contract for stale CLIs.
//
// This validator asserts:
//   1. Inside the brace-balanced RevokeOutcome::Ambiguous variant
//      span, the line immediately above `truncated: bool,` is exactly
//      `#[serde(default)]`.
//   2. The docs prose carries the `#[serde(default)]` literal verbatim
//      next to the truncated/Ambiguous citation.
//
// The Ambiguous variant span is located inside the
// `pub enum RevokeOutcome { ... }` block, scoped by brace-balance — so
// a future `#[serde(default)]` on a different variant's field cannot
// satisfy this pin.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-peer-auth/src/lib.rs";

const expectedAnnotation = "#[serde(default)]";
const fieldDeclaration = "truncated: bool,";
const citation = {
  regex:
    /\(see `RevokeOutcome::Ambiguous` at `covenant-peer-auth\/src\/lib\.rs:\d+-\d+`\)\. The field carries `#\[serde\(default\)\]`/,
  label: "RevokeOutcome::Ambiguous truncated #[serde(default)] literal citation",
  template:
    "(see `RevokeOutcome::Ambiguous` at `covenant-peer-auth/src/lib.rs:NN-MM`). The field carries `#[serde(default)]`",
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

let annotationLine = null;
if (source) {
  const lines = source.split("\n");
  const enumOpeners = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (/^pub\s+enum\s+RevokeOutcome\b/.test(lines[index])) {
      enumOpeners.push(index + 1);
    }
  }
  if (enumOpeners.length !== 1) {
    fail(
      `${sourcePath}: expected exactly 1 "pub enum RevokeOutcome" at top level but found ${enumOpeners.length}; remediation: confirm the RevokeOutcome enum exists at top level`,
    );
  } else {
    const enumStart = enumOpeners[0];
    const enumEnd = scanBraceBalance(lines, enumStart);
    if (enumEnd === null) {
      fail(
        `${sourcePath}: could not find the matching closing brace for "pub enum RevokeOutcome" starting at line ${enumStart}; remediation: confirm the enum body is brace-balanced`,
      );
    } else {
      const ambiguousOpeners = [];
      for (let index = enumStart; index < enumEnd; index += 1) {
        if (/^\s*Ambiguous\s*\{/.test(lines[index])) {
          ambiguousOpeners.push(index + 1);
        }
      }
      if (ambiguousOpeners.length !== 1) {
        fail(
          `${sourcePath}: expected exactly 1 "Ambiguous {" variant inside the RevokeOutcome enum (lines ${enumStart}-${enumEnd}) but found ${ambiguousOpeners.length}; remediation: confirm the Ambiguous variant exists and is not renamed or duplicated`,
        );
      } else {
        const ambiguousStart = ambiguousOpeners[0];
        const ambiguousEnd = scanBraceBalance(lines, ambiguousStart);
        if (ambiguousEnd === null) {
          fail(
            `${sourcePath}: could not find the matching closing brace for the Ambiguous variant starting at line ${ambiguousStart}; remediation: confirm the variant body is brace-balanced`,
          );
        } else {
          const fieldMatches = [];
          for (let index = ambiguousStart; index < ambiguousEnd; index += 1) {
            if (lines[index].trim() === fieldDeclaration) {
              fieldMatches.push(index + 1);
            }
          }
          if (fieldMatches.length !== 1) {
            fail(
              `${sourcePath}: expected exactly 1 "${fieldDeclaration}" inside the Ambiguous variant (lines ${ambiguousStart}-${ambiguousEnd}) but found ${fieldMatches.length}; remediation: confirm the truncated field exists and has not been renamed, retyped, or duplicated`,
            );
          } else {
            const fieldLine = fieldMatches[0];
            const candidate = fieldLine - 1;
            const text = lines[candidate - 1].trim();
            if (text !== expectedAnnotation) {
              fail(
                `${sourcePath}:${candidate}: expected exactly \`${expectedAnnotation}\` immediately above "${fieldDeclaration}" at line ${fieldLine}, but found \`${text}\`; remediation: restore the annotation directly above the truncated field, or update docs and source together if the wire format changed`,
              );
            } else {
              annotationLine = candidate;
            }
          }
        }
      }
    }
  }
}

if (docs) {
  if (!citation.regex.test(docs)) {
    fail(
      `${docsPath}: missing the ${citation.label} ("${citation.template}"); remediation: restore the docs sentence that records the Ambiguous variant range and the \`${expectedAnnotation}\` literal verbatim`,
    );
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-peer-auth-revoke-outcome-ambiguous-truncated-attribute-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-peer-auth-revoke-outcome-ambiguous-truncated-attribute-line-refs: ok (RevokeOutcome::Ambiguous.truncated #[serde(default)] lib.rs:${annotationLine})`,
);
