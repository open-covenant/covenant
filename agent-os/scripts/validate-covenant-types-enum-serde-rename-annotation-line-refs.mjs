#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// covenant-types enum #[serde(rename_all)] annotation line-ref drift guard.
// docs/ipc-and-http-gateway.md cites two annotation lines that sit
// immediately above their `pub enum` declarations in
// agent-os/crates/covenant-types/src/lib.rs:
//
//   - MemoryTier  `#[serde(rename_all = "lowercase")]` at lib.rs:23
//     (cited inside the memory_read tier field block, docs line 413)
//   - ResourceKind `#[serde(rename_all = "lowercase")]` at lib.rs:35
//     (cited inside the receipt resource field block, docs line 206)
//
// The existing covenant-types struct and AgentId Serialize impl
// validators explicitly exclude annotation-line citations because the
// anchor convention is different: the citation refers to the
// `#[serde(rename_all = ...)]` attribute on the line immediately above
// `pub enum <Name>`. The docs path style is `covenant-types/src/lib.rs:NN`
// (without the `agent-os/crates/` prefix), matching the impl/annotation
// citation convention.
//
// This validator derives each enum line at run time by matching
// `pub enum <Name>` with a strict top-level regex, then asserts the
// line immediately above contains the literal annotation text. The
// annotation line number is then asserted against the docs citation.

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

function read(path) {
  return readFileSync(join(repoRoot, path), "utf8").replace(/\r\n/g, "\n");
}

const docsPath = "docs/ipc-and-http-gateway.md";
const sourcePath = "agent-os/crates/covenant-types/src/lib.rs";

const targets = {
  MemoryTier: {
    expectedAnnotation: '#[serde(rename_all = "lowercase")]',
    enumLine: null,
    annotationLine: null,
    docsRegex:
      /per `MemoryTier`'s `#\[serde\(rename_all = "lowercase"\)\]` at `covenant-types\/src\/lib\.rs:(\d+)`/,
    docsLabel: "MemoryTier annotation citation",
    docsTemplate:
      "per `MemoryTier`'s `#[serde(rename_all = \"lowercase\")]` at `covenant-types/src/lib.rs:NN`",
  },
  ResourceKind: {
    expectedAnnotation: '#[serde(rename_all = "lowercase")]',
    enumLine: null,
    annotationLine: null,
    docsRegex:
      /lowercase per `#\[serde\(rename_all = "lowercase"\)\]` at `covenant-types\/src\/lib\.rs:(\d+)`/,
    docsLabel: "ResourceKind annotation citation",
    docsTemplate:
      "lowercase per `#[serde(rename_all = \"lowercase\")]` at `covenant-types/src/lib.rs:NN`",
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
  source = read(sourcePath);
} catch (error) {
  fail(`cannot read ${sourcePath}: ${error.message}`);
}

if (source) {
  const lines = source.split("\n");
  for (const name of Object.keys(targets)) {
    const matches = [];
    for (let index = 0; index < lines.length; index += 1) {
      const match = lines[index].match(/^pub\s+enum\s+(\w+)\b/);
      if (match && match[1] === name) {
        matches.push(index + 1);
      }
    }
    if (matches.length !== 1) {
      fail(
        `${sourcePath}: expected exactly 1 "pub enum ${name}" but found ${matches.length}; remediation: confirm the ${name} enum exists at top level in covenant-types/src/lib.rs and is not renamed, moved, or duplicated`,
      );
      continue;
    }
    const enumLine = matches[0];
    if (enumLine < 2) {
      fail(
        `${sourcePath}: "pub enum ${name}" is at line ${enumLine}; expected the #[serde(rename_all)] annotation on the preceding line, but the enum is at the top of the file`,
      );
      continue;
    }
    const annotationLine = enumLine - 1;
    const annotationText = lines[annotationLine - 1];
    if (annotationText !== targets[name].expectedAnnotation) {
      fail(
        `${sourcePath}:${annotationLine}: expected exactly \`${targets[name].expectedAnnotation}\` immediately above "pub enum ${name}" at line ${enumLine}, but found \`${annotationText}\`; remediation: restore the annotation on the line directly above the enum declaration`,
      );
      continue;
    }
    targets[name].enumLine = enumLine;
    targets[name].annotationLine = annotationLine;
  }
}

if (docs) {
  for (const [name, entry] of Object.entries(targets)) {
    const match = docs.match(entry.docsRegex);
    if (!match) {
      fail(
        `${docsPath}: missing the ${entry.docsLabel} ("${entry.docsTemplate}"); remediation: restore the citation that records the ${name} annotation line ref`,
      );
      continue;
    }
    if (entry.annotationLine !== null) {
      const cited = parseInt(match[1], 10);
      if (cited !== entry.annotationLine) {
        fail(
          `${docsPath}: the ${entry.docsLabel} cites covenant-types/src/lib.rs:${cited} but \`${entry.expectedAnnotation}\` for "pub enum ${name}" is at line ${entry.annotationLine}; remediation: update the citation to :${entry.annotationLine}`,
        );
      }
    }
  }
}

if (errors.length > 0) {
  console.error("validate-covenant-types-enum-serde-rename-annotation-line-refs: failed");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(
  `validate-covenant-types-enum-serde-rename-annotation-line-refs: ok (MemoryTier annotation lib.rs:${targets.MemoryTier.annotationLine}, ResourceKind annotation lib.rs:${targets.ResourceKind.annotationLine})`,
);
